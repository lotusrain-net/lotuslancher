// The app's commands go through the ACL: the hub view (a remote origin) may
// call only its `desktop_*` commands, granted at runtime for the control
// origin (shell::add_hub_capability); the local views get `shell_*` from
// capabilities/default.json. Any command not listed here is unreachable from
// every view. Kept in step with shell::commands (its test reads this file).
const COMMANDS: &[&str] = &[
    "runtime_local_status",
    "desktop_status",
    "desktop_engine_update_status",
    "desktop_device_context",
    "desktop_sign_challenge",
    "desktop_claim_grant",
    "desktop_logout",
    "desktop_cancel_logout",
    "desktop_open_settings",
    "desktop_hub_ready",
    "desktop_hub_offline",
    "desktop_hub_logout_ready",
    "shell_snapshot",
    "shell_set_drawer",
    "shell_reconnect",
    "shell_action",
    "shell_cleanup",
    "shell_preferences",
    "shell_set_control_url",
    "shell_set_autostart",
    "shell_set_system_proxy",
    "shell_summary",
    "shell_window",
    "shell_install_engine",
    "shell_quit",
];

fn main() {
    let attrs = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    tauri_build::try_build(attrs).expect("tauri build");
}
