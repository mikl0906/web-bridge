//! Run the bridge's HTTP/WebSocket server standalone, without the Tauri shell.
//!
//!   cargo run --example serve
//!
//! Includes registry/server by source path so the example doesn't link the
//! Tauri-bearing library (which needs WebView native libs at runtime). Used
//! for end-to-end testing the relay, REST, and MCP surfaces against a demo
//! app (see examples/ at the repo root).

#[path = "../src/registry.rs"]
mod registry;
#[path = "../src/server.rs"]
mod server;

use registry::AppState;

#[tokio::main]
async fn main() {
    let state = AppState::new();
    if let Err(e) = server::start(state).await {
        eprintln!("server failed: {e}");
    }
}
