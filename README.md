# Web Bridge

A small desktop app (Windows/macOS, Tauri) that acts as a **local host for web
apps**: a web app connects out to the bridge and exposes its operations, and
any local client — an agent, a script, an MCP client — can discover and call
them over plain HTTP. Everything stays on the machine.

```
┌────────────┐  WebSocket   ┌────────────┐  HTTP (REST / MCP)  ┌──────────┐
│  web app   │ ───────────► │   bridge   │ ◄────────────────── │  agent / │
│ (browser)  │   hello +    │ :7421      │   GET /apps         │  script  │
└────────────┘   tools      └────────────┘   POST /<app>/…     └──────────┘
```

The bridge never understands any app's domain — it relays tool calls to the
connected app and returns the reply. Browsers can't listen on ports, which is
the whole reason the bridge exists: the app dials *out* to `localhost:7421`,
and the bridge listens on its behalf.

## Repo layout

| Path          | What it is                                                        |
| ------------- | ----------------------------------------------------------------- |
| `web-bridge/` | The Tauri desktop app (React UI + axum server in `src-tauri/`)    |
| `sdk/`        | `web-bridge-sdk` — npm package web apps use to connect            |
| `examples/`   | `todo.mjs`, a minimal app connected through the SDK               |

## Quick start

```sh
# 1. Run the bridge (desktop app; or headless: cargo run --example serve)
cd web-bridge && npm install && npm run tauri dev

# 2. Connect an example app
cd sdk && npm install && npm run build
node examples/todo.mjs

# 3. Discover and call it
curl http://localhost:7421/apps
curl -X POST http://localhost:7421/todo/api/addTask \
     -H 'content-type: application/json' -d '{"title":"buy milk"}'

# or add it to an MCP client (instance id shown in the bridge window / /apps):
claude mcp add --transport http todo http://localhost:7421/todo/<instanceId>/mcp
```

## HTTP surface

The server binds `127.0.0.1:7421` (scanning upward if taken; the UI shows the
real port). CORS is permissive — it's loopback-only.

| Route                                | What it does                                                   |
| ------------------------------------ | -------------------------------------------------------------- |
| `GET /` or `GET /apps`               | Discovery: every connected app, description, instructions, per-instance endpoint URLs, tool names |
| `GET /status`                        | Light status snapshot (used by the bridge UI)                  |
| `GET /ws/app`                        | WebSocket endpoint apps connect to                             |
| `POST /{app}/{instance}/mcp`         | MCP server for that instance (Streamable HTTP, JSON responses) |
| `POST /{app}/{instance}/api/{tool}`  | Plain REST: body = tool payload, response = tool result        |
| `GET  /{app}/{instance}/tools`       | Full tool catalog with input schemas, no MCP handshake needed  |
| `POST /{app}/mcp`, `/{app}/api/{tool}`, `GET /{app}/tools` | Shorthand while exactly one instance of the app is connected (`409` + instance list when ambiguous) |

Errors are `{ "error": { "message": … } }` with meaningful statuses: `404`
unknown app/instance, `409` ambiguous shorthand, `422` the app rejected the
call (its error payload is passed through), `503` app disconnected mid-call,
`504` app didn't answer within 30 s. Over MCP, app-level failures come back as
tool results with `isError: true` so agents can read and recover.

## Wire protocol (app ⇄ bridge)

A single WebSocket, JSON text frames. Apps normally use the SDK and never see
this; any WebSocket client can implement it.

1. **`hello`** (app → bridge, must be first; may be re-sent anytime to refresh
   metadata/tools):

   ```json
   {
     "type": "hello",
     "protocolVersion": 1,
     "app": "todo",
     "title": "Todo",
     "description": "What the app is — shown in discovery",
     "instructions": "How agents should use the tools — served via MCP initialize",
     "instanceId": "optional stable slug; omit for a random one",
     "instanceLabel": "e.g. the open document's name",
     "tools": [
       {
         "name": "addTask",
         "description": "…",
         "inputSchema": { "type": "object", "properties": { … } },
         "annotations": { "readOnlyHint": false, "destructiveHint": false }
       }
     ]
   }
   ```

   `app` and `instanceId` are URL path segments: lowercase slugs
   (`[a-z0-9][a-z0-9_-]{0,63}`), not one of the reserved words `api`, `mcp`,
   `ws`, `apps`, `status`, `tools`.

2. **`welcome`** (bridge → app): confirms identity and endpoints —
   `{ "type": "welcome", "app", "instanceId", "baseUrl", "mcpUrl", "apiUrl" }`.

3. **Relayed calls** (bridge → app): `{ "id", "action", "payload" }`. The app
   replies `{ "id", "ok": true, "result": … }` or
   `{ "id", "ok": false, "error": { "message", … } }`.

4. **`error`** (bridge → app): the connection is being rejected or closed —
   malformed hello, invalid slug, a UI disconnect, or another connection
   claiming the same `(app, instanceId)`. Terminal: the SDK stops reconnecting
   when it receives one.

Instance identity is **last-wins**: a new connection with the same
`(app, instanceId)` replaces the old one (this is what makes page reloads
seamless), and the old connection is told why. Give each browser tab its own
id (sessionStorage, not localStorage) if you persist ids.

## Design notes

- **The tool catalog lives in the app**, not the bridge. Adding an operation
  to an app surfaces it over REST and MCP with no bridge changes.
- **REST and MCP hit the same registry**: `POST …/api/<tool>` and MCP
  `tools/call` are the same dispatch, so there is one behavior to test.
- **State belongs to apps.** The bridge holds no data; if an app reloads, its
  state is whatever the app restored.
- The MCP implementation is the minimal Streamable-HTTP slice (`initialize`,
  `ping`, `tools/list`, `tools/call`, JSON responses, no SSE) — the bridge
  never initiates messages.

## Development

```sh
# hub server without the Tauri shell (fast iteration, e2e tests)
cd web-bridge/src-tauri && cargo run --example serve

# desktop app
cd web-bridge && npm run tauri dev

# production bundles (dmg / msi / exe)
cd web-bridge && npm run tauri build
```
