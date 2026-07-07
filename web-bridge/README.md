# Web Bridge — desktop app

The Tauri app: a status window (React) plus the bridge server (axum, in
`src-tauri/src/`). See the [repo README](../README.md) for the architecture
and wire protocol.

- `src-tauri/src/registry.rs` — connected-instance registry and request/reply
  relay (the broker).
- `src-tauri/src/server.rs` — HTTP/WebSocket surface: `/ws/app`, discovery,
  per-instance REST + MCP routes.
- `src/App.tsx` — the window: server status, connected apps, copyable
  endpoints, per-instance disconnect.

```sh
npm install
npm run tauri dev                 # run the desktop app
npm run tauri build               # produce installers (dmg/msi/exe)

cd src-tauri
cargo run --example serve         # server only, no window (for tests/CI)
```
