pub mod registry;
pub mod server;

use registry::AppState;
use serde_json::Value;

/// Status snapshot for the bridge window (port, running, connected apps).
#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Value {
    state.status_json()
}

/// Close one app connection from the bridge side.
#[tauri::command]
fn disconnect_instance(state: tauri::State<'_, AppState>, app: String, instance: String) {
    state.request_disconnect(&app, &instance);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = AppState::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state.clone())
        .setup(move |_app| {
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = server::start(state).await {
                    eprintln!("web-bridge server failed: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_status, disconnect_instance])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
