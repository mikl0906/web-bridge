//! The axum HTTP/WebSocket server.
//!
//! One loopback port serves three audiences:
//!   * `GET  /ws/app` — web apps connect out (WebSocket) and announce
//!     themselves with a `hello` frame.
//!   * `POST /{app}/{instance}/mcp` — MCP clients (Streamable HTTP, JSON
//!     responses), one server per connected app instance.
//!   * `POST /{app}/{instance}/api/{action}` — plain REST dispatch.
//!
//! `GET /apps` is the discovery document: every connected app, its
//! description/instructions, and per-instance endpoint URLs. When exactly one
//! instance of an app is connected, the shorthand `/{app}/mcp` and
//! `/{app}/api/{action}` resolve to it.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

use crate::registry::{AppState, Hello, HubError, Instance, Outbound};

/// Preferred port; we scan upward from here if it's taken.
const PREFERRED_PORT: u16 = 7421;

/// How long a fresh WebSocket may sit silent before its `hello` is due.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Bind a loopback port (preferring [`PREFERRED_PORT`]) and serve forever.
/// Records the chosen port in `state` before serving so the UI can show it.
pub async fn start(state: AppState) -> std::io::Result<()> {
    let (listener, port) = bind_port().await?;
    state.set_port(port);
    println!("web-bridge listening on http://127.0.0.1:{port}");

    let app = Router::new()
        .route("/", get(root))
        .route("/status", get(status))
        .route("/apps", get(apps))
        .route("/ws/app", get(ws_app))
        // Shorthand routes: valid while exactly one instance of the app is
        // connected. Static segments ("mcp", "api", "tools") take precedence
        // over the `{instance}` param in the full routes below.
        .route("/{app}/mcp", post(mcp_app))
        .route("/{app}/tools", get(tools_app))
        .route("/{app}/api/{action}", post(api_app))
        // Fully-qualified per-instance routes.
        .route("/{app}/{instance}/mcp", post(mcp_instance))
        .route("/{app}/{instance}/tools", get(tools_instance))
        .route("/{app}/{instance}/api/{action}", post(api_instance))
        // Local-only tool; allow any origin so browser/agent clients work.
        .layer(CorsLayer::permissive())
        .with_state(state);

    axum::serve(listener, app).await
}

async fn bind_port() -> std::io::Result<(tokio::net::TcpListener, u16)> {
    for port in PREFERRED_PORT..PREFERRED_PORT + 20 {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            return Ok((l, port));
        }
    }
    // Fall back to an OS-assigned ephemeral port.
    let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = l.local_addr()?.port();
    Ok((l, port))
}

fn base_url(state: &AppState) -> String {
    format!("http://localhost:{}", state.port())
}

// ---------------------------------------------------------------------------
// Discovery + status
// ---------------------------------------------------------------------------

async fn root(State(state): State<AppState>) -> Json<Value> {
    // Same document as /apps; the root exists so `curl localhost:7421` is
    // enough to find everything.
    Json(state.apps_json(&base_url(&state)))
}

async fn apps(State(state): State<AppState>) -> Json<Value> {
    Json(state.apps_json(&base_url(&state)))
}

async fn status(State(state): State<AppState>) -> Json<Value> {
    Json(state.status_json())
}

// ---------------------------------------------------------------------------
// App WebSocket
// ---------------------------------------------------------------------------

async fn ws_app(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| app_socket(socket, state))
}

async fn app_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();

    // The first frame must be a `hello` announcing who this is.
    let hello: Hello = loop {
        let frame = tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await;
        let text = match frame {
            Ok(Some(Ok(Message::Text(t)))) => t,
            // Ignore leading non-text frames (pings etc.) within the timeout.
            Ok(Some(Ok(_))) => continue,
            _ => return,
        };
        match parse_hello(&text) {
            Ok(h) => break h,
            Err(msg) => {
                let _ = sink
                    .send(Message::Text(
                        json!({ "type": "error", "message": msg }).to_string().into(),
                    ))
                    .await;
                return;
            }
        }
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<Outbound>();
    let instance = match state.register(&hello, tx) {
        Ok(i) => i,
        Err(msg) => {
            let _ = sink
                .send(Message::Text(
                    json!({ "type": "error", "message": msg }).to_string().into(),
                ))
                .await;
            return;
        }
    };

    // Tell the app who it is and where it's exposed.
    let base = format!("{}/{}/{}", base_url(&state), instance.app, instance.id);
    let welcome = json!({
        "type": "welcome",
        "app": instance.app,
        "instanceId": instance.id,
        "baseUrl": base,
        "mcpUrl": format!("{base}/mcp"),
        "apiUrl": format!("{base}/api"),
    });
    if sink
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        state.remove(&instance);
        return;
    }

    // Pump registry -> app. A `Close` control message (UI disconnect, or a new
    // connection taking over this instance id) ends the connection; any
    // farewell frame queued before it goes out first.
    let send_task = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            match out {
                Outbound::Frame(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Outbound::Close => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    // Pump app -> registry.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(t) => state.handle_app_message(&instance, &t),
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    state.remove(&instance);
}

fn parse_hello(text: &str) -> Result<Hello, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("Malformed hello frame: {e}"))?;
    if value.get("type").and_then(Value::as_str) != Some("hello") {
        return Err("The first frame must be a hello: {\"type\":\"hello\",\"app\":\"<name>\", …}".into());
    }
    serde_json::from_value(value).map_err(|e| format!("Malformed hello frame: {e}"))
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/// Map a `HubError` to an HTTP status + JSON `{ error }` body.
fn hub_error_response(e: HubError) -> Response {
    let code = match e {
        HubError::NotFound { .. } => StatusCode::NOT_FOUND,
        HubError::Ambiguous { .. } => StatusCode::CONFLICT,
        HubError::Gone => StatusCode::SERVICE_UNAVAILABLE,
        HubError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        HubError::App(_) => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (code, Json(json!({ "error": e.to_json() }))).into_response()
}

async fn call_rest(state: &AppState, instance: &Instance, action: &str, payload: Value) -> Response {
    match state.call(instance, action, payload).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => hub_error_response(e),
    }
}

/// Dispatch any operation by name: the last path segment is the operation and
/// the JSON body is its payload — the same registry MCP `tools/call` reaches.
/// `{}` is a fine body for operations that take no arguments.
async fn api_instance(
    State(state): State<AppState>,
    Path((app, instance, action)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Response {
    match state.get(&app, &instance) {
        Some(inst) => call_rest(&state, &inst, &action, body).await,
        None => hub_error_response(HubError::NotFound { app, instance: Some(instance) }),
    }
}

async fn api_app(
    State(state): State<AppState>,
    Path((app, action)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    match state.resolve_single(&app) {
        Ok(inst) => call_rest(&state, &inst, &action, body).await,
        Err(e) => hub_error_response(e),
    }
}

/// Full tool catalog (names, descriptions, input schemas) as plain JSON, for
/// clients that want it without an MCP handshake.
async fn tools_instance(
    State(state): State<AppState>,
    Path((app, instance)): Path<(String, String)>,
) -> Response {
    match state.get(&app, &instance) {
        Some(inst) => Json(json!({ "tools": inst.tools() })).into_response(),
        None => hub_error_response(HubError::NotFound { app, instance: Some(instance) }),
    }
}

async fn tools_app(State(state): State<AppState>, Path(app): Path<String>) -> Response {
    match state.resolve_single(&app) {
        Ok(inst) => Json(json!({ "tools": inst.tools() })).into_response(),
        Err(e) => hub_error_response(e),
    }
}

// ---------------------------------------------------------------------------
// MCP (Streamable HTTP, JSON responses)
// ---------------------------------------------------------------------------
//
// We implement the small slice of the MCP spec we need: a single POST endpoint
// per app instance that handles JSON-RPC `initialize`, `tools/list`,
// `tools/call`, and `ping`, answering with `application/json` (no SSE — the
// bridge never initiates messages). Notifications (no `id`) get 202.

const PROTOCOL_VERSION: &str = "2025-06-18";

async fn mcp_instance(
    State(state): State<AppState>,
    Path((app, instance)): Path<(String, String)>,
    Json(req): Json<Value>,
) -> Response {
    match state.get(&app, &instance) {
        Some(inst) => mcp(&state, &inst, req).await,
        None => hub_error_response(HubError::NotFound { app, instance: Some(instance) }),
    }
}

async fn mcp_app(
    State(state): State<AppState>,
    Path(app): Path<String>,
    Json(req): Json<Value>,
) -> Response {
    match state.resolve_single(&app) {
        Ok(inst) => mcp(&state, &inst, req).await,
        Err(e) => hub_error_response(e),
    }
}

async fn mcp(state: &AppState, instance: &Instance, req: Value) -> Response {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");

    // JSON-RPC notifications carry no id and expect no response body.
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = id.unwrap();

    match method {
        "initialize" => {
            let mut result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": format!("web-bridge:{}", instance.app),
                    "version": env!("CARGO_PKG_VERSION")
                }
            });
            // The app's usage instructions surface through MCP's standard
            // `instructions` field, shown to the model by most clients.
            if let Some(instructions) = instance.instructions() {
                result["instructions"] = Value::String(instructions);
            }
            json_rpc_result(id, result)
        }
        "ping" => json_rpc_result(id, json!({})),
        // The connected app advertises its tool catalog in the `hello` frame;
        // we serve it verbatim.
        "tools/list" => json_rpc_result(id, json!({ "tools": instance.tools() })),
        "tools/call" => mcp_tools_call(state, instance, id, req.get("params")).await,
        other => json_rpc_error(id, -32601, &format!("Method not found: {other}")),
    }
}

async fn mcp_tools_call(
    state: &AppState,
    instance: &Instance,
    id: Value,
    params: Option<&Value>,
) -> Response {
    let Some(params) = params else {
        return json_rpc_error(id, -32602, "Missing params");
    };
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        return json_rpc_error(id, -32602, "Missing tool name");
    }
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    // Every tool maps 1:1 onto an app operation; the tool name is the action
    // and the arguments are the payload. We don't whitelist names here — the
    // app's operation registry is the source of truth and returns a clear
    // error for an unknown operation, surfaced below as an MCP tool error.
    match state.call(instance, name, args).await {
        Ok(result) => json_rpc_result(id, tool_text(&result, false)),
        // Surface domain errors as an MCP tool error so the agent can read and
        // recover, rather than a transport-level JSON-RPC error.
        Err(e) => json_rpc_result(id, tool_text(&e.to_json(), true)),
    }
}

/// Wrap a value as MCP `tools/call` content (JSON serialized into a text part).
fn tool_text(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "isError": is_error
    })
}

fn json_rpc_result(id: Value, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    }))
    .into_response()
}
