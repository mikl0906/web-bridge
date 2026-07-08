# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Web Bridge is a local relay: web apps (which can't listen on ports) dial *out* over WebSocket to a desktop hub on `localhost:7421`, announce a tool catalog, and the bridge exposes those tools to local clients over plain REST and MCP (Streamable HTTP). The bridge is domain-agnostic — it relays opaque JSON payloads and correlates replies by request id. Everything stays loopback-only.

## Commands

```sh
# Hub server WITHOUT the Tauri shell — fastest iteration loop, no WebView needed
cd web-bridge/src-tauri && cargo run --example serve

# Full desktop app (Tauri dev mode, hot-reloads the React UI)
cd web-bridge && npm run tauri dev

# Production bundles (dmg / msi / exe)
cd web-bridge && npm run tauri build

# SDK (must be built before running examples — examples import sdk/dist/)
cd sdk && npm install && npm run build

# End-to-end smoke test: run the serve example, then
node examples/todo.mjs            # connect the demo app
curl http://localhost:7421/apps   # discovery
curl -X POST http://localhost:7421/todo/api/addTask -H 'content-type: application/json' -d '{"title":"x"}'

# Rust checks
cd web-bridge/src-tauri && cargo check && cargo clippy
```

There is no automated test suite. Verification is manual/e2e: `cargo run --example serve` + `examples/todo.mjs` + curl against the HTTP surface.

## Layout

- `web-bridge/` — Tauri desktop app: React status UI (`src/`) + the actual server in Rust (`src-tauri/src/`)
- `sdk/` — `web-bridge-sdk`, the zero-dependency npm package web apps use to connect (single file: `sdk/src/index.ts`)
- `examples/todo.mjs` — minimal demo app over the SDK (needs Node ≥ 22 for global WebSocket)

## Architecture

All server logic is two Rust files:

- `web-bridge/src-tauri/src/registry.rs` — `AppState`: the instance map keyed by `(app, instanceId)`, hello parsing/refresh, request/response correlation (uuid → oneshot channel, 30 s timeout), `HubError` taxonomy, and the discovery/status JSON documents.
- `web-bridge/src-tauri/src/server.rs` — axum router: `/ws/app` WebSocket handler (hello handshake, welcome frame, socket pumps), REST dispatch (`/{app}[/{instance}]/api/{tool}`), the minimal MCP slice (`initialize`, `ping`, `tools/list`, `tools/call`; JSON responses only, no SSE), and `HubError` → HTTP status mapping (404/409/422/503/504).

`lib.rs` is only the Tauri shell: spawns the server, exposes `get_status` and `disconnect_instance` commands to the React UI, which polls `get_status` every second.

**Dual compilation:** `src-tauri/examples/serve.rs` includes `registry.rs` and `server.rs` via `#[path]` so the headless server compiles without linking the Tauri library (which needs WebView at runtime). Consequence: those two modules must stay free of Tauri dependencies, and anything used only by the Tauri path may need `#[allow(dead_code)]` (see `request_disconnect`).

**Wire protocol** (documented in the root README, mirrored in `sdk/src/index.ts`): `hello` (app→bridge, first frame, re-sendable to refresh metadata/tools) → `welcome` (bridge→app, confirms identity + URLs) → relayed calls `{id, action, payload}` answered by `{id, ok, result|error}`. An `error` frame from the bridge is terminal — the SDK stops reconnecting on it.

**Key invariants** — the protocol lives in three places that must stay in sync (registry/server, SDK, README):

- Instance identity is **last-wins**: a new connection with the same `(app, instanceId)` replaces the old one (this is what makes page reloads seamless); the old socket gets a farewell `error` frame before close. Cleanup is guarded by a monotonic `conn_id` so a stale socket's teardown can't remove its replacement.
- App names and instance ids are URL path segments: slugs `[a-z0-9][a-z0-9_-]{0,63}`, with reserved words `api, mcp, ws, apps, status, tools`. `RESERVED_SLUGS` and the slug regex are duplicated in `registry.rs` and `sdk/src/index.ts`.
- REST `POST …/api/<tool>` and MCP `tools/call` are the **same dispatch** (`AppState::call`) — one behavior to test. Tool names map 1:1 to actions; the bridge never validates tool names or payloads (the app is the source of truth and its errors pass through as 422 / MCP `isError: true`).
- The `/{app}/…` shorthand (no instance id) resolves only while exactly one instance is connected; otherwise 409 with the instance list.
- The bridge holds no app state and never initiates MCP messages.

The server binds `127.0.0.1:7421`, scanning upward ~20 ports if taken, then falling back to an ephemeral port; the SDK's `?bridgePort=` URL param / `port` option is how apps follow it.
