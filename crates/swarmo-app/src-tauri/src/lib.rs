//! The Swarmo desktop application.

pub mod commands;
pub mod scripting;
pub mod state;

use tauri::Manager;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "swarmo_app=info,swarmo_load=info,warn".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let temp_dir = app
                .path()
                .app_cache_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join("responses");
            let state = AppState::new(temp_dir);

            if let Ok(dir) = app.path().app_config_dir() {
                state.load_settings(dir.join("settings.json"));
                state.load_approvals(dir.join("approvals.json"));
            }

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // workspace
            commands::workspace::workspace_open,
            commands::workspace::workspace_create,
            commands::workspace::workspace_info,
            commands::workspace::workspace_recent,
            commands::workspace::workspace_close,
            commands::workspace::tree_get,
            // environments
            commands::workspace::env_list,
            commands::workspace::env_get,
            commands::workspace::env_save,
            commands::workspace::env_create,
            commands::workspace::env_delete,
            commands::workspace::env_set_active,
            commands::workspace::env_effective,
            commands::workspace::runtime_vars_clear,
            commands::workspace::token_preview,
            // requests
            commands::workspace::request_get,
            commands::workspace::request_save,
            commands::workspace::request_create,
            commands::workspace::request_rename,
            commands::workspace::request_duplicate,
            commands::workspace::request_move,
            // containers
            commands::workspace::collection_create,
            commands::workspace::folder_create,
            commands::workspace::container_get,
            commands::workspace::container_save,
            commands::workspace::container_rename,
            commands::workspace::load_tree,
            commands::workspace::load_folder_create,
            commands::workspace::load_folder_rename,
            commands::workspace::load_test_move,
            commands::workspace::load_test_rename,
            commands::workspace::load_test_duplicate,
            commands::workspace::load_test_locate,
            commands::workspace::request_locate,
            commands::workspace::environment_rename,
            commands::workspace::node_delete,
            // import
            commands::workspace::import_postman_collection,
            commands::workspace::import_postman_environment,
            commands::workspace::auth_command_test,
            commands::workspace::auth_command_approve,
            commands::workspace::auth_commands_approved,
            commands::workspace::curl_parse,
            commands::workspace::curl_import,
            // settings
            commands::workspace::settings_get,
            commands::workspace::settings_save,
            commands::workspace::approved_hosts_clear,
            // gRPC
            commands::grpc_exec::grpc_request_get,
            commands::grpc_exec::grpc_request_save,
            commands::grpc_exec::grpc_request_create,
            commands::grpc_exec::grpc_list_services,
            commands::grpc_exec::grpc_message_template,
            commands::grpc_exec::grpc_send,
            commands::ws_exec::ws_request_get,
            commands::ws_exec::ws_request_save,
            commands::ws_exec::ws_request_create,
            commands::ws_exec::ws_send,
            // execution
            commands::exec::request_send,
            commands::exec::request_resolve_url,
            commands::exec::request_cancel,
            commands::exec::history_list,
            commands::exec::history_clear,
            commands::exec::history_delete,
            commands::exec::cookies_list,
            commands::exec::cookies_clear,
            // load tests
            commands::workspace::load_list,
            commands::load::scenario_get,
            commands::load::scenario_save,
            commands::load::scenario_create,
            commands::load::user_script_create,
            commands::load::user_script_template,
            commands::load::text_get,
            commands::load::text_save,
            commands::load::load_promote,
            commands::load::load_preflight,
            commands::load::load_approve_hosts,
            commands::load::load_run,
            commands::load::load_stop,
            commands::load::load_active_runs,
            // runs
            commands::load::runs_list,
            commands::load::run_get,
            commands::load::run_timeline,
            commands::load::run_export,
            commands::load::run_annotation_get,
            commands::load::run_annotation_set,
            commands::load::run_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Swarmo");
}
