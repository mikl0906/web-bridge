//! The multi-app relay registry.
//!
//! The bridge never understands any app's domain. A web app connects out over
//! a WebSocket and announces itself with a `hello` frame (app name, optional
//! instance id, description, MCP instructions, tool catalog). The registry
//! tracks every connected instance under an `(app, instance)` key, forwards
//! client requests to the right socket, correlates replies by request id, and
//! hands results back to whichever HTTP client (REST or MCP) asked. All
//! payloads and results are opaque `serde_json::Value`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// How long a client call waits for the app to respond before giving up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Path segments with routing meaning; apps and instances can't claim them.
const RESERVED_SLUGS: &[&str] = &["api", "mcp", "ws", "apps", "status", "tools"];

/// What the registry hands the socket task to write out. Using a control
/// message (rather than a side-channel signal) keeps ordering: a farewell
/// frame queued before `Close` is guaranteed to reach the app first.
pub enum Outbound {
    /// A text frame to send verbatim.
    Frame(String),
    /// Send a WebSocket close and end the connection.
    Close,
}

/// Request sent bridge -> app over the WebSocket.
#[derive(Serialize)]
struct Request<'a> {
    id: String,
    action: &'a str,
    payload: Value,
}

/// Response sent app -> bridge over the WebSocket.
#[derive(Deserialize)]
pub struct WireResponse {
    id: String,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Value,
}

/// The `hello` frame an app sends as its first message, and may re-send at any
/// time to refresh its metadata/tool catalog (e.g. after opening a document).
#[derive(Deserialize)]
pub struct Hello {
    pub app: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default, rename = "instanceId")]
    pub instance_id: Option<String>,
    #[serde(default, rename = "instanceLabel")]
    pub instance_label: Option<String>,
    #[serde(default)]
    pub tools: Option<Value>,
}

/// Why a client call could not be fulfilled.
#[derive(Debug)]
pub enum HubError {
    /// No connected instance matches the requested path.
    NotFound { app: String, instance: Option<String> },
    /// The `/{app}/...` shorthand was used while several instances of that app
    /// are connected; carries their ids so the client can pick one.
    Ambiguous { app: String, instances: Vec<String> },
    /// The app disconnected before answering.
    Gone,
    /// The app did not answer within [`REQUEST_TIMEOUT`].
    Timeout,
    /// The app answered with `ok: false`; carries its `error` payload
    /// (e.g. a validation failure with `message` + `issues`).
    App(Value),
}

impl HubError {
    /// JSON body surfaced to API/MCP clients.
    pub fn to_json(&self) -> Value {
        match self {
            HubError::NotFound { app, instance } => {
                let target = match instance {
                    Some(i) => format!("{app}/{i}"),
                    None => app.clone(),
                };
                json!({
                    "message": format!(
                        "No connected app matches '{target}'. Fetch GET /apps to list what is connected."
                    )
                })
            }
            HubError::Ambiguous { app, instances } => json!({
                "message": format!(
                    "Several instances of '{app}' are connected; address one explicitly as /{app}/<instanceId>/…"
                ),
                "instances": instances,
            }),
            HubError::Gone => json!({
                "message": "The app disconnected before answering."
            }),
            HubError::Timeout => json!({
                "message": "The app did not respond in time."
            }),
            HubError::App(e) => e.clone(),
        }
    }
}

/// Mutable per-instance metadata, refreshed whenever the app re-sends `hello`.
#[derive(Clone)]
struct InstanceInfo {
    title: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
    label: Option<String>,
    /// MCP tool catalog (a JSON array) the app advertised.
    tools: Value,
}

impl InstanceInfo {
    fn from_hello(hello: &Hello) -> Self {
        InstanceInfo {
            title: hello.title.clone(),
            description: hello.description.clone(),
            instructions: hello.instructions.clone(),
            label: hello.instance_label.clone(),
            tools: hello
                .tools
                .clone()
                .filter(Value::is_array)
                .unwrap_or_else(|| Value::Array(vec![])),
        }
    }
}

/// One live app connection.
pub struct Instance {
    /// Monotonic connection id; guards cleanup against removing a replacement
    /// that reused the same `(app, instance)` key (e.g. a page reload).
    conn_id: u64,
    pub app: String,
    pub id: String,
    pub connected_at: u64,
    info: Mutex<InstanceInfo>,
    /// Sender feeding this instance's WebSocket.
    sender: mpsc::UnboundedSender<Outbound>,
    /// In-flight client calls awaiting a reply, keyed by request id.
    pending: Mutex<HashMap<String, oneshot::Sender<WireResponse>>>,
}

impl Instance {
    /// The advertised MCP tool catalog (a JSON array).
    pub fn tools(&self) -> Value {
        self.info.lock().unwrap().tools.clone()
    }

    pub fn instructions(&self) -> Option<String> {
        self.info.lock().unwrap().instructions.clone()
    }

    /// Wake every in-flight waiter with failure (dropping the oneshot sender
    /// resolves their `rx` with a RecvError, mapped to `HubError::Gone`).
    fn fail_pending(&self) {
        self.pending.lock().unwrap().clear();
    }

    /// Queue a farewell explaining the close, then the close itself.
    fn kick(&self, message: &str) {
        let _ = self.sender.send(Outbound::Frame(
            json!({ "type": "error", "message": message }).to_string(),
        ));
        let _ = self.sender.send(Outbound::Close);
        self.fail_pending();
    }
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    instances: Mutex<HashMap<(String, String), Arc<Instance>>>,
    /// The port the server actually bound to (set once at startup).
    port: AtomicU16,
    conn_counter: AtomicU64,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            inner: Arc::new(Inner {
                instances: Mutex::new(HashMap::new()),
                port: AtomicU16::new(0),
                conn_counter: AtomicU64::new(0),
            }),
        }
    }

    pub fn set_port(&self, port: u16) {
        self.inner.port.store(port, Ordering::Relaxed);
    }

    pub fn port(&self) -> u16 {
        self.inner.port.load(Ordering::Relaxed)
    }

    /// Register a freshly connected app from its `hello` frame. If the same
    /// `(app, instance)` key is already live (a reload reconnecting before the
    /// old socket noticed), the new connection wins; the old one is told it
    /// was replaced (so a still-live client stops reconnecting rather than
    /// fighting over the id) and closed.
    pub fn register(
        &self,
        hello: &Hello,
        sender: mpsc::UnboundedSender<Outbound>,
    ) -> Result<Arc<Instance>, String> {
        let app = validate_slug(&hello.app, "app name")?;
        let mut map = self.inner.instances.lock().unwrap();
        let id = match &hello.instance_id {
            Some(id) => validate_slug(id, "instance id")?,
            None => loop {
                let candidate = short_id();
                if !map.contains_key(&(app.clone(), candidate.clone())) {
                    break candidate;
                }
            },
        };
        let instance = Arc::new(Instance {
            conn_id: self.inner.conn_counter.fetch_add(1, Ordering::Relaxed) + 1,
            app: app.clone(),
            id: id.clone(),
            connected_at: unix_now(),
            info: Mutex::new(InstanceInfo::from_hello(hello)),
            sender,
            pending: Mutex::new(HashMap::new()),
        });
        if let Some(old) = map.insert((app, id), instance.clone()) {
            old.kick(&format!(
                "Another connection claimed '{}/{}'. If this is a second tab of the same app, use a per-tab instance id (e.g. sessionStorage, not localStorage).",
                instance.app, instance.id
            ));
        }
        Ok(instance)
    }

    /// Drop an instance when its socket closes. A no-op if the key was already
    /// taken over by a newer connection.
    pub fn remove(&self, instance: &Arc<Instance>) {
        let key = (instance.app.clone(), instance.id.clone());
        let mut map = self.inner.instances.lock().unwrap();
        if map.get(&key).map(|i| i.conn_id) == Some(instance.conn_id) {
            map.remove(&key);
        }
        drop(map);
        instance.fail_pending();
    }

    pub fn get(&self, app: &str, instance: &str) -> Option<Arc<Instance>> {
        self.inner
            .instances
            .lock()
            .unwrap()
            .get(&(app.to_owned(), instance.to_owned()))
            .cloned()
    }

    /// Resolve the `/{app}/...` shorthand: succeeds only when exactly one
    /// instance of the app is connected.
    pub fn resolve_single(&self, app: &str) -> Result<Arc<Instance>, HubError> {
        let map = self.inner.instances.lock().unwrap();
        let mut matches: Vec<&Arc<Instance>> =
            map.values().filter(|i| i.app == app).collect();
        match matches.len() {
            0 => Err(HubError::NotFound { app: app.to_owned(), instance: None }),
            1 => Ok(matches.pop().unwrap().clone()),
            _ => Err(HubError::Ambiguous {
                app: app.to_owned(),
                instances: matches.iter().map(|i| i.id.clone()).collect(),
            }),
        }
    }

    /// Ask a live connection to close (triggered from the bridge UI).
    /// (dead_code allowed: the headless `serve` example compiles this module
    /// without the Tauri command that calls it.)
    #[allow(dead_code)]
    pub fn request_disconnect(&self, app: &str, instance: &str) {
        if let Some(inst) = self.get(app, instance) {
            inst.kick("Disconnected from the bridge.");
        }
    }

    /// Route a message from an app: a `hello` frame refreshes its metadata;
    /// anything else is a reply to a pending call, matched by request id.
    pub fn handle_app_message(&self, instance: &Instance, text: &str) {
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bridge: malformed message from {}/{}: {e}", instance.app, instance.id);
                return;
            }
        };

        if value.get("type").and_then(Value::as_str) == Some("hello") {
            // A re-hello refreshes metadata and tools; the identity (app name,
            // instance id) is fixed for the lifetime of the connection.
            if let Ok(hello) = serde_json::from_value::<Hello>(value) {
                *instance.info.lock().unwrap() = InstanceInfo::from_hello(&hello);
            }
            return;
        }

        let resp: WireResponse = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("bridge: malformed message from {}/{}: {e}", instance.app, instance.id);
                return;
            }
        };
        if let Some(tx) = instance.pending.lock().unwrap().remove(&resp.id) {
            let _ = tx.send(resp);
        }
    }

    /// Relay one action to an app instance and await its reply.
    pub async fn call(
        &self,
        instance: &Instance,
        action: &str,
        payload: Value,
    ) -> Result<Value, HubError> {
        let id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        instance.pending.lock().unwrap().insert(id.clone(), tx);

        let req = Request { id: id.clone(), action, payload };
        let text = serde_json::to_string(&req).expect("request serializes");
        if instance.sender.send(Outbound::Frame(text)).is_err() {
            instance.pending.lock().unwrap().remove(&id);
            return Err(HubError::Gone);
        }

        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(resp)) if resp.ok => Ok(resp.result),
            Ok(Ok(resp)) => Err(HubError::App(resp.error)),
            // Sender dropped (app disconnected mid-flight).
            Ok(Err(_)) => Err(HubError::Gone),
            Err(_) => {
                instance.pending.lock().unwrap().remove(&id);
                Err(HubError::Timeout)
            }
        }
    }

    /// Every connected instance, grouped by app, ordered by connection time.
    fn grouped(&self) -> Vec<(String, Vec<Arc<Instance>>)> {
        let map = self.inner.instances.lock().unwrap();
        let mut all: Vec<Arc<Instance>> = map.values().cloned().collect();
        drop(map);
        all.sort_by_key(|i| (i.app.clone(), i.connected_at, i.conn_id));
        let mut groups: Vec<(String, Vec<Arc<Instance>>)> = Vec::new();
        for inst in all {
            match groups.last_mut() {
                Some((app, list)) if *app == inst.app => list.push(inst),
                _ => groups.push((inst.app.clone(), vec![inst])),
            }
        }
        groups
    }

    /// The discovery document served at `GET /apps` (and embedded in `/`).
    /// `base` is e.g. `http://localhost:7421`.
    pub fn apps_json(&self, base: &str) -> Value {
        let apps: Vec<Value> = self
            .grouped()
            .into_iter()
            .map(|(app, instances)| {
                // App-level metadata lives on each instance's hello; the most
                // recently connected one is the freshest source.
                let latest = instances.last().unwrap().info.lock().unwrap().clone();
                let instances: Vec<Value> = instances
                    .iter()
                    .map(|i| {
                        let info = i.info.lock().unwrap();
                        let tools: Vec<Value> = info
                            .tools
                            .as_array()
                            .map(|t| t.iter().map(tool_summary).collect())
                            .unwrap_or_default();
                        json!({
                            "instanceId": i.id,
                            "label": info.label,
                            "connectedAt": i.connected_at,
                            "mcpUrl": format!("{base}/{app}/{}/mcp", i.id),
                            "apiUrl": format!("{base}/{app}/{}/api", i.id),
                            "toolsUrl": format!("{base}/{app}/{}/tools", i.id),
                            "tools": tools,
                        })
                    })
                    .collect();
                json!({
                    "app": app,
                    "title": latest.title,
                    "description": latest.description,
                    "instructions": latest.instructions,
                    "instances": instances,
                })
            })
            .collect();
        json!({
            "service": "web-bridge",
            "version": env!("CARGO_PKG_VERSION"),
            "connectUrl": format!("{}/ws/app", base.replacen("http", "ws", 1)),
            "apps": apps,
        })
    }

    /// Status snapshot for the bridge window (port, running, connected apps).
    pub fn status_json(&self) -> Value {
        let port = self.port();
        let apps: Vec<Value> = self
            .grouped()
            .into_iter()
            .map(|(app, instances)| {
                let latest = instances.last().unwrap().info.lock().unwrap().clone();
                let instances: Vec<Value> = instances
                    .iter()
                    .map(|i| {
                        let info = i.info.lock().unwrap();
                        json!({
                            "instanceId": i.id,
                            "label": info.label,
                            "connectedAt": i.connected_at,
                            "toolCount": info.tools.as_array().map(Vec::len).unwrap_or(0),
                        })
                    })
                    .collect();
                json!({
                    "app": app,
                    "title": latest.title,
                    "description": latest.description,
                    "instances": instances,
                })
            })
            .collect();
        json!({
            "running": port != 0,
            "port": port,
            "apps": apps,
        })
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Discovery lists tool names + descriptions only; full input schemas come
/// from `GET .../tools` or MCP `tools/list`.
fn tool_summary(tool: &Value) -> Value {
    json!({
        "name": tool.get("name"),
        "description": tool.get("description"),
    })
}

/// App names and instance ids become URL path segments, so they are
/// constrained to a lowercase slug and must not shadow reserved routes.
fn validate_slug(s: &str, what: &str) -> Result<String, String> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars().next().unwrap().is_ascii_alphanumeric()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        return Err(format!(
            "Invalid {what} '{s}': use 1-64 chars of [a-z0-9_-], starting with a letter or digit."
        ));
    }
    if RESERVED_SLUGS.contains(&s) {
        return Err(format!("Invalid {what} '{s}': that name is reserved."));
    }
    Ok(s.to_owned())
}

/// Short random id for instances that don't bring their own.
fn short_id() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_owned()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
