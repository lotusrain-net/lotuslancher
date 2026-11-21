//! The shell's commands. Two sets, two capabilities:
//!
//! * `desktop_*` — the hub view's, allowed only from the control origin in the
//!   `hub` webview (runtime capability `hub`). They return states and codes;
//!   no path, port, secret or private key ever reaches the remote page.
//! * `shell_*` — the local `main` and `drawer` views' (capability `local`).
//!
//! Every command also checks the calling webview's label itself, so a
//! capability mistake cannot hand one set to the other view.

use std::time::Duration;

use serde_json::{json, Value};
use tauri::Webview;

use super::{agent, autostart, state};

/// For build.rs's AppManifest and the hub capability.
pub const HUB_COMMANDS: &[&str] = &[
    "desktop_status",
    "desktop_device_context",
    "desktop_sign_challenge",
    "desktop_claim_grant",
    "desktop_logout",
    "desktop_cancel_logout",
    "desktop_open_settings",
    "desktop_hub_ready",
    "desktop_hub_offline",
    "desktop_hub_logout_ready",
];

const SHORT: Duration = Duration::from_secs(5);
const LONG: Duration = Duration::from_secs(100);

fn hub_only(w: &Webview) -> Result<(), String> {
    let origin_ok = w.url().ok().is_some_and(|u| super::is_control(&u));
    if w.label() == "hub" && origin_ok {
        Ok(())
    } else {
        Err("forbidden".into())
    }
}

fn local_only(w: &Webview) -> Result<(), String> {
    if matches!(w.label(), "main" | "drawer") {
        Ok(())
    } else {
        Err("forbidden".into())
    }
}

fn code(e: anyhow::Error) -> String {
    agent::sanitize_code(&e.to_string())
}

/// A user id from the page: a UUID, nothing else forwarded.
fn user_id(raw: &str) -> Result<String, String> {
    let ok = raw.len() == 36
        && raw.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    if ok {
        Ok(raw.to_ascii_lowercase())
    } else {
        Err("invalid_user_id".into())
    }
}

fn short_text(raw: &str, max: usize) -> Result<String, String> {
    if raw.is_empty() || raw.len() > max || raw.chars().any(|c| c.is_control()) {
        Err("invalid_request".into())
    } else {
        Ok(raw.to_string())
    }
}

// ---- hub ----

#[tauri::command]
pub fn desktop_status(webview: Webview) -> Result<Value, String> {
    hub_only(&webview)?;
    Ok(super::hub_status())
}

#[tauri::command]
pub async fn desktop_device_context(webview: Webview, user_id: String) -> Result<Value, String> {
    hub_only(&webview)?;
    let uid = self::user_id(&user_id)?;
    let v = agent::call("POST", "/desktop/context", Some(&json!({ "user_id": uid })), SHORT).await.map_err(code)?;
    Ok(json!({
        "device_id": v.get("device_id"),
        "iat": v.get("iat"),
        "exp": v.get("exp"),
        "assertion": v.get("assertion"),
    }))
}

/// Sign the server's challenge with the device key the agent holds. The
/// answer is what the grant request carries: public key and signature only.
#[tauri::command]
pub async fn desktop_sign_challenge(webview: Webview, challenge_id: String, challenge_nonce: String, user_id: String) -> Result<Value, String> {
    hub_only(&webview)?;
    let body = json!({
        "challenge_id": short_text(&challenge_id, 128)?,
        "challenge_nonce": short_text(&challenge_nonce, 256)?,
        "user_id": self::user_id(&user_id)?,
    });
    agent::call("POST", "/desktop/challenge/sign", Some(&body), SHORT).await.map_err(code)
}

#[tauri::command]
pub async fn desktop_claim_grant(webview: Webview, grant_ticket: String, expires_at: Value) -> Result<(), String> {
    hub_only(&webview)?;
    let body = json!({ "grant_ticket": short_text(&grant_ticket, 512)?, "expires_at": expires_at });
    agent::call("POST", "/desktop/grants/claim", Some(&body), SHORT).await.map_err(code)?;
    Ok(())
}

/// Latch the wipe first: whatever happens after, the next start finishes it.
#[tauri::command]
pub async fn desktop_logout(webview: Webview) -> Result<(), String> {
    hub_only(&webview)?;
    start_logout().await
}

pub async fn start_logout() -> Result<(), String> {
    state::update(|s| s.pending_webview_clear = true).map_err(|_| "state_write_failed".to_string())?;
    super::logout_requested();
    if let Err(e) = agent::call("POST", "/desktop/logout", None, SHORT).await {
        // Nothing started: no wipe owed.
        let _ = state::update(|s| s.pending_webview_clear = false);
        super::logout_cancelled();
        return Err(code(e));
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_cancel_logout(webview: Webview) -> Result<(), String> {
    hub_only(&webview)?;
    agent::call("POST", "/desktop/cancel-logout", None, SHORT).await.map_err(code)?;
    let _ = state::update(|s| s.pending_webview_clear = false);
    super::logout_cancelled();
    Ok(())
}

#[tauri::command]
pub fn desktop_open_settings(webview: Webview) -> Result<(), String> {
    hub_only(&webview)?;
    super::open_settings();
    Ok(())
}

#[tauri::command]
pub fn desktop_hub_ready(webview: Webview) -> Result<(), String> {
    hub_only(&webview)?;
    super::hub_ready();
    Ok(())
}

#[tauri::command]
pub fn desktop_hub_offline(webview: Webview) -> Result<(), String> {
    hub_only(&webview)?;
    super::hub_offline();
    Ok(())
}

#[tauri::command]
pub fn desktop_hub_logout_ready(webview: Webview) -> Result<(), String> {
    hub_only(&webview)?;
    super::hub_logout_ack();
    Ok(())
}

// ---- local views ----

#[tauri::command]
pub fn shell_snapshot(webview: Webview) -> Result<Value, String> {
    local_only(&webview)?;
    Ok(super::snapshot_value())
}

#[tauri::command]
pub fn shell_set_drawer(webview: Webview, open: bool) -> Result<(), String> {
    local_only(&webview)?;
    super::set_drawer(open);
    Ok(())
}

/// The offline page's 「重新连接」: the agent's reconnect, then the hub again.
#[tauri::command]
pub async fn shell_reconnect(webview: Webview) -> Result<(), String> {
    local_only(&webview)?;
    let _ = agent::call("POST", "/desktop/action", Some(&json!({ "name": "reconnect" })), LONG).await;
    super::retry_hub();
    Ok(())
}

/// The drawer's buttons. `collect_logs` reveals the zip it wrote; `remove`
/// starts the uninstaller and quits.
#[tauri::command]
pub async fn shell_action(webview: Webview, name: String) -> Result<Value, String> {
    local_only(&webview)?;
    if !matches!(name.as_str(), "reconnect" | "probe" | "driver_retry" | "retry_cleanup" | "collect_logs" | "logs" | "remove") {
        return Err("action_not_allowed".into());
    }
    let v = agent::call("POST", "/desktop/action", Some(&json!({ "name": name })), LONG).await.map_err(code)?;
    let result = v.get("result").cloned().unwrap_or(Value::Null);
    match name.as_str() {
        "collect_logs" => {
            if let Some(p) = result.as_str() {
                let _ = tauri_plugin_opener::reveal_item_in_dir(p);
            }
            Ok(json!({ "ok": true }))
        }
        "remove" => {
            super::quit();
            Ok(json!({ "ok": true }))
        }
        "reconnect" => {
            super::retry_hub();
            Ok(json!({ "ok": true }))
        }
        // Agent messages are its own fixed Chinese strings, no paths.
        _ => Ok(json!({ "ok": true, "message": result.as_str() })),
    }
}

#[tauri::command]
pub async fn shell_cleanup(webview: Webview) -> Result<(), String> {
    local_only(&webview)?;
    agent::call("POST", "/desktop/cleanup", None, LONG).await.map_err(code)?;
    Ok(())
}

#[tauri::command]
pub async fn shell_preferences(webview: Webview) -> Result<Value, String> {
    local_only(&webview)?;
    let p = agent::call("GET", "/desktop/preferences", None, SHORT).await.unwrap_or(Value::Null);
    Ok(json!({
        "autostart_available": autostart::AVAILABLE,
        "autostart": autostart::enabled(),
        "use_system_proxy_for_control": p.get("use_system_proxy_for_control").and_then(Value::as_bool).unwrap_or(false),
    }))
}

#[tauri::command]
pub async fn shell_set_autostart(webview: Webview, on: bool) -> Result<(), String> {
    local_only(&webview)?;
    autostart::set(on).map_err(code)
}

#[tauri::command]
pub async fn shell_set_system_proxy(webview: Webview, on: bool) -> Result<(), String> {
    local_only(&webview)?;
    let body = json!({ "use_system_proxy_for_control": on });
    agent::call("POST", "/desktop/preferences/save", Some(&body), LONG).await.map_err(code)?;
    Ok(())
}

/// 「复制版本号与状态摘要」: versions and states, no ids or paths.
#[tauri::command]
pub fn shell_summary(webview: Webview) -> Result<String, String> {
    local_only(&webview)?;
    let s = super::hub_status();
    let agent_version = super::with(|sh| sh.status.get("version").and_then(Value::as_str).map(str::to_owned)).unwrap_or_else(|| "-".into());
    let field = |k: &str| match s.get(k) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => "-".into(),
    };
    Ok(format!(
        "LotusProxy {} / agent {} / {} {}\nauthorization={} connection={} driver_ready={} engine_ready={} session_active={} cleanup_pending={} error={}",
        env!("CARGO_PKG_VERSION"),
        agent_version,
        std::env::consts::OS,
        std::env::consts::ARCH,
        field("authorization"),
        field("connection"),
        field("driver_ready"),
        field("engine_ready"),
        field("session_active"),
        field("cleanup_pending"),
        field("error"),
    ))
}

#[tauri::command]
pub fn shell_window(webview: Webview, op: String) -> Result<(), String> {
    local_only(&webview)?;
    let w = webview.window();
    match op.as_str() {
        "minimize" => w.minimize().map_err(|_| "window_failed".to_string()),
        "toggle_maximize" => {
            let max = w.is_maximized().unwrap_or(false);
            if max { w.unmaximize() } else { w.maximize() }.map_err(|_| "window_failed".to_string())
        }
        "close" => {
            super::hide_to_tray();
            Ok(())
        }
        "drag" => w.start_dragging().map_err(|_| "window_failed".to_string()),
        _ => Err("invalid_request".into()),
    }
}

#[tauri::command]
pub fn shell_install_engine(webview: Webview) -> Result<(), String> {
    local_only(&webview)?;
    super::install_engine();
    Ok(())
}

#[tauri::command]
pub fn shell_quit(webview: Webview) -> Result<(), String> {
    local_only(&webview)?;
    super::quit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_uuid_is_forwarded_as_a_user_id() {
        assert!(user_id("0b8f2a5e-1c2d-4e3f-8a9b-0c1d2e3f4a5b").is_ok());
        for bad in ["", "1", "../../etc", "0b8f2a5e-1c2d-4e3f-8a9b-0c1d2e3f4a5bx", "0b8f2a5e_1c2d_4e3f_8a9b_0c1d2e3f4a5b"] {
            assert!(user_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn build_rs_lists_every_shell_command() {
        let build = include_str!("../../build.rs");
        let lib = include_str!("../lib.rs");
        for c in HUB_COMMANDS {
            assert!(build.contains(&format!("\"{c}\"")), "{c} missing from build.rs");
        }
        for line in lib.lines().map(str::trim).filter(|l| l.starts_with("shell::shell_") || l.starts_with("shell::desktop_")) {
            let c = line.trim_start_matches("shell::").trim_end_matches(',');
            assert!(build.contains(&format!("\"{c}\"")), "{c} missing from build.rs");
        }
    }

    #[test]
    fn hub_commands_are_all_desktop_ones() {
        assert!(HUB_COMMANDS.iter().all(|c| c.starts_with("desktop_")));
    }
}
