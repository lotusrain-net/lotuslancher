//! LotusProxy's one window: three webviews side by side, never overlapping.
//!
//! * `main`   local: the 36 px title bar, or the whole window for the first-run,
//!            loading, offline and quitting screens.
//! * `drawer` local: the 360 px settings drawer on the right.
//! * `hub`    remote: the LotusHub control origin, below the title bar. Its own
//!            data folder (`hub-webview`), its own capability (`desktop_*`
//!            commands only), navigation pinned to the control origin.
//!
//! The shell also supervises the embedded agent and runs the last stage of a
//! desktop logout: wiping the hub view's storage, also at the next start when
//! a crash left `pending_webview_clear` set.

pub mod agent;
pub mod autostart;
pub mod commands;
pub mod state;

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl};

pub use commands::*;

pub const TITLE_H: f64 = 36.0;
pub const DRAWER_W: f64 = 360.0;
const ENGINE_BANNER_H: f64 = 52.0;
/// How long a hub page may take to say it loaded before the offline screen.
const HUB_READY_TIMEOUT: Duration = Duration::from_secs(30);
const HUB_OFFLINE_GRACE: Duration = Duration::from_secs(4);
const AGENT_STATUS_GRACE: Duration = Duration::from_secs(15);
/// Covers the agent's 3 min cleanup budget plus its UI close and worker drain.
const QUIT_HARD_CAP: Duration = Duration::from_secs(210);
/// How long the hub gets to end its IAM session before the wipe goes ahead.
const LOGOUT_ACK_WAIT: Duration = Duration::from_secs(5);
const CONTROL_OVERRIDE: Option<&str> = option_env!("LOTUS_CONTROL_URL");
const DEFAULT_CONTROL: &str = "https://hub.lotusrain.net";

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    /// Agent starting, hub page loading: a quiet full-window splash.
    Starting,
    /// No browser engine yet: the preparation page.
    Init,
    /// Title bar + hub (+ drawer).
    Daily,
    /// The hub page did not load: the offline page, never the engine's error page.
    Offline,
    /// Quitting with cleanup to finish.
    Quitting,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AgentHealth {
    Checking,
    Healthy,
    Retrying,
    Offline,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngineUpdate {
    Checking,
    Current,
    Downloading,
    Updated,
    BusyWaiting,
    BlockedLauncher,
    FailedNetwork,
    FailedManual,
}

struct Shell {
    screen: Screen,
    drawer: bool,
    control: Option<url::Url>,
    hub_ready: bool,
    /// Bumped on every hub navigation, so a late timer of an earlier one is ignored.
    hub_round: u64,
    /// Bumped on every offline/online/load transition to cancel stale timers.
    hub_offline_round: u64,
    status: Value,
    agent_health: AgentHealth,
    agent_failure_since: Option<Instant>,
    engine_update: EngineUpdate,
    engine_required_version: Option<String>,
    engine_min_launcher_version: Option<String>,
    engine_update_error: Option<Value>,
    last_engine_hint_check: Option<Instant>,
    logout_requested: Option<Instant>,
    logout_seen: bool,
    hub_logout_ack: bool,
}

static SHELL: Mutex<Shell> = Mutex::new(Shell {
    screen: Screen::Starting,
    drawer: false,
    control: None,
    hub_ready: false,
    hub_round: 0,
    hub_offline_round: 0,
    status: Value::Null,
    agent_health: AgentHealth::Checking,
    agent_failure_since: None,
    engine_update: EngineUpdate::Checking,
    engine_required_version: None,
    engine_min_launcher_version: None,
    engine_update_error: None,
    last_engine_hint_check: None,
    logout_requested: None,
    logout_seen: false,
    hub_logout_ack: false,
});
static APP: OnceLock<AppHandle> = OnceLock::new();
static WIPING: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);
static QUIT_DONE: AtomicBool = AtomicBool::new(false);
static CHECKING_ENGINE_UPDATE: AtomicBool = AtomicBool::new(false);
static ENGINE_BUSY_RETRY_PENDING: AtomicBool = AtomicBool::new(false);
static ENGINE_BUSY_RETRY_USED: AtomicBool = AtomicBool::new(false);

fn with<T>(f: impl FnOnce(&mut Shell) -> T) -> T {
    let mut g = SHELL.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

fn app() -> Option<&'static AppHandle> {
    APP.get()
}

/// A URL is the control origin's own: same scheme, host and port.
pub fn is_control(url: &url::Url) -> bool {
    with(|s| {
        s.control
            .as_ref()
            .is_some_and(|c| c.origin() == url.origin())
    })
}

/// Accept the agent's control URL: https, or http on loopback for development.
fn parse_control(raw: &str) -> Option<url::Url> {
    parse_control_with_local_http(raw, cfg!(debug_assertions))
}

/// A compile-time debugging override may use an HTTP loopback dev server, even
/// in release builds. It cannot redirect the shell to a non-local HTTP host.
fn parse_control_override(raw: &str) -> Option<url::Url> {
    parse_control_with_local_http(raw, true)
}

fn parse_control_with_local_http(raw: &str, allow_local_http: bool) -> Option<url::Url> {
    let u = url::Url::parse(raw).ok()?;
    let host = u.host_str()?;
    let loopback = host == "127.0.0.1" || host == "localhost" || host == "[::1]";
    let ok = (u.scheme() == "https" || (u.scheme() == "http" && loopback && allow_local_http))
        && u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none();
    ok.then(|| url::Url::parse(&u.origin().ascii_serialization()).ok())
        .flatten()
}

/// An explicit development target wins over the agent's configured endpoint.
/// An invalid override fails closed instead of silently opening production.
fn resolve_control_with_setting(
    build_override: Option<&str>,
    settings_override: Option<&str>,
    agent_url: Option<&str>,
) -> Option<url::Url> {
    if let Some(raw) = build_override {
        return parse_control_override(raw);
    }
    if let Some(raw) = settings_override.filter(|raw| !raw.trim().is_empty()) {
        return parse_control_with_local_http(raw, true);
    }
    agent_url
        .and_then(parse_control)
        .or_else(|| parse_control(DEFAULT_CONTROL))
}

fn resolve_control(override_url: Option<&str>, agent_url: Option<&str>) -> Option<url::Url> {
    resolve_control_with_setting(override_url, None, agent_url)
}

// ---- layout ----

#[derive(Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, PartialEq)]
pub struct Layout {
    pub main: Rect,
    pub drawer: Option<Rect>,
    pub hub: Option<Rect>,
}

/// Where each view goes for a window of `w` x `h` logical pixels.
pub fn layout(screen: Screen, drawer: bool, w: f64, h: f64) -> Layout {
    layout_with_engine_banner(screen, drawer, w, h, false)
}

fn layout_with_engine_banner(
    screen: Screen,
    drawer: bool,
    w: f64,
    h: f64,
    engine_banner: bool,
) -> Layout {
    let body = (h - TITLE_H).max(0.0);
    match screen {
        Screen::Daily => {
            let banner_h = if engine_banner { ENGINE_BANNER_H } else { 0.0 };
            let drawer_w = if drawer { DRAWER_W.min(w) } else { 0.0 };
            Layout {
                main: Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h: (TITLE_H + banner_h).min(h),
                },
                drawer: drawer.then(|| Rect {
                    x: w - drawer_w,
                    y: TITLE_H + banner_h,
                    w: drawer_w,
                    h: (body - banner_h).max(0.0),
                }),
                hub: Some(Rect {
                    x: 0.0,
                    y: TITLE_H + banner_h,
                    w: (w - drawer_w).max(0.0),
                    h: (body - banner_h).max(0.0),
                }),
            }
        }
        // The full-window pages draw their own drawer when asked for one.
        _ => Layout {
            main: Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            drawer: None,
            hub: None,
        },
    }
}

fn place(v: &Webview, r: &Rect) {
    let _ = v.set_position(LogicalPosition::new(r.x, r.y));
    let _ = v.set_size(LogicalSize::new(r.w, r.h));
}

/// Put every view where the current screen wants it and tell the local views.
pub fn relayout() {
    let Some(app) = app() else { return };
    let Some(win) = app.get_window("main") else {
        return;
    };
    let (screen, drawer, update) = with(|s| (s.screen, s.drawer, s.engine_update));
    let scale = win.scale_factor().unwrap_or(1.0);
    let Ok(size) = win.inner_size() else { return };
    let size = size.to_logical::<f64>(scale);
    let banner = !matches!(update, EngineUpdate::Checking | EngineUpdate::Current);
    let l = layout_with_engine_banner(screen, drawer, size.width, size.height, banner);
    if let Some(v) = app.get_webview("main") {
        place(&v, &l.main);
    }
    if let Some(v) = app.get_webview("drawer") {
        match &l.drawer {
            Some(r) => {
                place(&v, r);
                let _ = v.show();
            }
            None => {
                let _ = v.hide();
            }
        }
    }
    if let Some(v) = app.get_webview("hub") {
        match &l.hub {
            Some(r) => {
                place(&v, r);
                let _ = v.show();
            }
            None => {
                let _ = v.hide();
            }
        }
    }
    emit_local("shell://screen", snapshot_value());
}

fn emit_local(event: &str, payload: Value) {
    if let Some(app) = app() {
        for label in ["main", "drawer"] {
            let _ = app.emit_to(tauri::EventTarget::webview(label), event, payload.clone());
        }
    }
}

pub fn set_screen(screen: Screen) {
    let changed = with(|s| {
        // Quitting is final.
        if s.screen == Screen::Quitting || s.screen == screen {
            return false;
        }
        s.screen = screen;
        true
    });
    if changed {
        relayout();
    }
}

/// How long the drawer view's content takes to leave (shell.css
/// `--shell-exit`, plus a frame): the view hides once it has.
const DRAWER_EXIT: Duration = Duration::from_millis(170);

pub fn set_drawer(open: bool) {
    let was = with(|s| std::mem::replace(&mut s.drawer, open));
    if was && !open {
        // Tell the views first: the drawer plays its exit, the gear lets go.
        // The layout (hide the drawer, widen the hub) follows the exit, unless
        // the drawer was opened again in the meantime.
        emit_local("shell://screen", snapshot_value());
        std::thread::spawn(|| {
            std::thread::sleep(DRAWER_EXIT);
            if !with(|s| s.drawer) {
                relayout();
            }
        });
        return;
    }
    relayout();
    if open {
        if let Some(v) = app().and_then(|a| a.get_webview("drawer")) {
            let _ = v.set_focus();
        }
    }
}

// ---- what the views see ----

/// The local views' picture: screen, drawer, the agent's status, versions.
pub fn snapshot_value() -> Value {
    let (
        screen,
        drawer,
        status,
        control,
        agent_health,
        engine_update,
        engine_required_version,
        engine_min_launcher_version,
        engine_update_error,
    ) = with(|s| {
        (
            s.screen,
            s.drawer,
            s.status.clone(),
            s.control.clone(),
            s.agent_health,
            s.engine_update,
            s.engine_required_version.clone(),
            s.engine_min_launcher_version.clone(),
            s.engine_update_error.clone(),
        )
    });
    let maximized = app()
        .and_then(|a| a.get_window("main"))
        .and_then(|w| w.is_maximized().ok())
        .unwrap_or(false);
    serde_json::json!({
        "screen": screen,
        "drawer": drawer,
        "maximized": maximized,
        "status": status,
        "agent_health": agent_health,
        "engine_ready": engine_ready(),
        "engine_installing": INSTALLING.load(SeqCst),
        "engine_error": ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "engine_update": engine_update,
        "engine_installed_version": crate::runtime::engine_version(),
        "engine_required_version": engine_required_version,
        "min_launcher_version": engine_min_launcher_version,
        "engine_update_error": engine_update_error,
        "shell_version": env!("CARGO_PKG_VERSION"),
        "control_host": control.as_ref().and_then(|u| u.host_str().map(str::to_owned)),
        "control_url": control.as_ref().map(|u| u.as_str()),
        "autostart_available": autostart::AVAILABLE,
    })
}

/// The remote hub can read update state without receiving local agent details.
pub fn engine_update_snapshot() -> Value {
    let snapshot = snapshot_value();
    serde_json::json!({
        "engine_update": snapshot.get("engine_update"),
        "engine_installed_version": snapshot.get("engine_installed_version"),
        "engine_required_version": snapshot.get("engine_required_version"),
        "engine_update_error": snapshot.get("engine_update_error"),
        "engine_installing": snapshot.get("engine_installing"),
        "min_launcher_version": snapshot.get("min_launcher_version"),
    })
}

/// The agent's status as the hub may see it: states and codes, no paths,
/// ports, URLs or ids.
pub fn hub_status() -> Value {
    let s = with(|s| s.status.clone());
    let pick = |k: &str| s.get(k).cloned().unwrap_or(Value::Null);
    serde_json::json!({
        "available": !s.is_null(),
        "authorization": pick("authorization"),
        "connection": pick("connection"),
        "transport": pick("transport"),
        "transport_error": pick("transport_error"),
        "transport_failure_since": pick("transport_failure_since"),
        "error_source": pick("error_source"),
        "error_phase": pick("error_phase"),
        "last_presence_at": pick("last_presence_at"),
        "session_active": pick("session_active"),
        "cleanup_pending": pick("cleanup_pending"),
        "cleanup_phase": pick("cleanup_phase"),
        "cleanup_state": pick("cleanup_state"),
        "pending_logout": pick("pending_logout"),
        "logout_stage": pick("logout_stage"),
        "pending_grant": pick("pending_grant"),
        "engine_update_hint": pick("engine_update_hint"),
        "engine_update_blocked_launcher": pick("engine_update_blocked_launcher"),
        "engine_required_version": pick("engine_required_version"),
        "error": s.get("error").and_then(Value::as_str).map(agent::sanitize_code).filter(|c| c != "operation_failed"),
    })
}

static INSTALLING: AtomicBool = AtomicBool::new(false);
static ENGINE_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Download the browser engine (first run, or the init page's retry). Its
/// progress reaches the local views as `runtime:progress`.
pub fn install_engine() {
    install_engine_inner(false);
}

/// Let the init page retry fingerprint downloads after the engine was already installed.
pub fn retry_engine_install() {
    if engine_ready() {
        let _ = state::update(|s| s.engine_update_manual_hold = false);
        tauri::async_runtime::spawn(check_engine_update_inner(true, false, true));
    } else {
        install_engine_inner(true);
    }
}

fn install_engine_inner(allow_installed_engine: bool) {
    if (!allow_installed_engine && engine_ready()) || INSTALLING.swap(true, SeqCst) {
        return;
    }
    *ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
    emit_local("shell://status", snapshot_value());
    tauri::async_runtime::spawn(async {
        let win = app().and_then(|a| a.get_window("main"));
        let res = match win {
            Some(w) => crate::runtime::runtime_install(w, false).await.map(|_| ()),
            None => Err("window_missing".into()),
        };
        INSTALLING.store(false, SeqCst);
        match res {
            Ok(()) => {
                // The agent probes the driver again now that it has an engine.
                let _ = agent::call(
                    "POST",
                    "/desktop/action",
                    Some(&serde_json::json!({ "name": "probe" })),
                    Duration::from_secs(100),
                )
                .await;
                let ready = with(|s| s.hub_ready);
                if ready {
                    set_screen(Screen::Daily);
                } else if with(|s| s.screen) == Screen::Init {
                    set_screen(Screen::Starting);
                    retry_hub();
                }
            }
            Err(e) => {
                eprintln!("[lotus] engine install failed: {e}");
                *ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some("engine_install_failed".into());
            }
        }
        emit_local("shell://status", snapshot_value());
    });
}

/// Reconcile an installed engine with the published manifest without holding
/// up the hub or existing sessions.
pub async fn check_engine_update() {
    check_engine_update_inner(false, false, false).await;
}

async fn check_engine_update_inner(bypass_cooldown: bool, busy_retry: bool, manual: bool) {
    if CHECKING_ENGINE_UPDATE.swap(true, SeqCst) {
        return;
    }
    if INSTALLING.load(SeqCst) || !engine_ready() {
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }

    let persisted = state::load();
    if persisted.engine_update_manual_hold && !manual {
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }
    if persisted.engine_update_failures >= 3
        && !bypass_cooldown
        && now_millis().saturating_sub(persisted.last_engine_check_ms) < ENGINE_CHECK_INTERVAL_MS
    {
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }
    if !busy_retry {
        ENGINE_BUSY_RETRY_PENDING.store(false, SeqCst);
        ENGINE_BUSY_RETRY_USED.store(false, SeqCst);
    }
    set_engine_update_state(EngineUpdate::Checking, None, None, None);

    let status = match crate::runtime::runtime_status().await {
        Ok(status) => status,
        Err(error) => {
            finish_network_failure(None, error, manual);
            CHECKING_ENGINE_UPDATE.store(false, SeqCst);
            return;
        }
    };
    let required = status.required_engine_version.clone();
    let min_launcher = status.min_launcher_version.clone();

    if status.needs_newer_launcher {
        record_engine_check(false, false);
        publish_engine_launcher_blocked(true, required.as_deref()).await;
        set_engine_update_state(EngineUpdate::BlockedLauncher, required, min_launcher, None);
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }
    publish_engine_launcher_blocked(false, None).await;
    if status.spec.is_none() {
        record_engine_check(false, false);
        set_engine_update_state(EngineUpdate::Current, required, min_launcher, None);
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }
    if status.remote_browser_etag.is_none() {
        finish_network_failure(required, "manifest_unavailable".into(), manual);
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }
    if !status.update_available {
        record_engine_check(false, false);
        set_engine_update_state(EngineUpdate::Current, required, min_launcher, None);
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }

    let Some(window) = app().and_then(|a| a.get_window("main")) else {
        finish_network_failure(required, "window_missing".into(), manual);
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    };
    if INSTALLING.swap(true, SeqCst) {
        CHECKING_ENGINE_UPDATE.store(false, SeqCst);
        return;
    }
    set_engine_update_state(
        EngineUpdate::Downloading,
        required.clone(),
        min_launcher.clone(),
        None,
    );
    let previous_etag = status.installed_browser_etag.clone();
    let installed = crate::runtime::runtime_install(window, false).await;
    INSTALLING.store(false, SeqCst);

    match installed {
        Ok(updated) => {
            if let Some(error) = updated.install_error {
                finish_install_failure(error, required, min_launcher, busy_retry);
            } else {
                record_engine_check(false, false);
                let moved = previous_etag != updated.installed_browser_etag;
                if moved {
                    // Re-probe the driver so its next heartbeat reports the new engine.
                    let _ = agent::call(
                        "POST",
                        "/desktop/action",
                        Some(&serde_json::json!({ "name": "probe" })),
                        Duration::from_secs(100),
                    )
                    .await;
                    set_engine_update_state(
                        EngineUpdate::Updated,
                        updated.required_engine_version,
                        updated.min_launcher_version,
                        None,
                    );
                } else {
                    set_engine_update_state(
                        EngineUpdate::Current,
                        updated.required_engine_version,
                        updated.min_launcher_version,
                        None,
                    );
                }
            }
        }
        Err(error) => {
            let latest = crate::runtime::runtime_status().await.ok();
            if let Some(install_error) = latest.as_ref().and_then(|s| s.install_error.clone()) {
                finish_install_failure(install_error, required, min_launcher, busy_retry);
            } else {
                finish_network_failure(required, error, manual);
            }
        }
    }
    CHECKING_ENGINE_UPDATE.store(false, SeqCst);
}

const ENGINE_CHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn record_engine_check(network_failure: bool, manual_hold: bool) {
    let _ = state::update(|saved| {
        saved.last_engine_check_ms = now_millis();
        if network_failure {
            saved.engine_update_failures = saved.engine_update_failures.saturating_add(1);
        } else {
            saved.engine_update_failures = 0;
        }
        saved.engine_update_manual_hold = manual_hold;
    });
}

fn set_engine_update_state(
    update: EngineUpdate,
    required: Option<String>,
    min_launcher: Option<String>,
    error: Option<Value>,
) {
    with(|shell| {
        shell.engine_update = update;
        shell.engine_required_version = required;
        shell.engine_min_launcher_version = min_launcher;
        shell.engine_update_error = error;
    });
    if matches!(
        update,
        EngineUpdate::FailedNetwork | EngineUpdate::FailedManual
    ) {
        *ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
            Some("engine_install_failed".into());
    } else {
        *ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    relayout();
}

fn finish_network_failure(required: Option<String>, message: String, _manual: bool) {
    record_engine_check(true, false);
    set_engine_update_state(
        EngineUpdate::FailedNetwork,
        required,
        None,
        Some(serde_json::json!({ "stage": "download", "message": message })),
    );
}

fn finish_install_failure(
    error: crate::runtime::InstallError,
    required: Option<String>,
    min_launcher: Option<String>,
    busy_retry: bool,
) {
    match error.stage.as_str() {
        "busy" => {
            record_engine_check(false, false);
            if !busy_retry {
                ENGINE_BUSY_RETRY_USED.store(false, SeqCst);
                ENGINE_BUSY_RETRY_PENDING.store(true, SeqCst);
            }
            set_engine_update_state(
                EngineUpdate::BusyWaiting,
                required,
                min_launcher,
                serde_json::to_value(error).ok(),
            );
        }
        "download" => {
            record_engine_check(true, false);
            set_engine_update_state(
                EngineUpdate::FailedNetwork,
                required,
                min_launcher,
                serde_json::to_value(error).ok(),
            );
        }
        stage => {
            let manual_hold = matches!(stage, "space" | "verify");
            record_engine_check(false, manual_hold);
            set_engine_update_state(
                EngineUpdate::FailedManual,
                required,
                min_launcher,
                serde_json::to_value(error).ok(),
            );
        }
    }
}

fn should_check_engine_from_tray() -> bool {
    let saved = state::load();
    !saved.engine_update_manual_hold
        && (saved.last_engine_check_ms == 0
            || now_millis().saturating_sub(saved.last_engine_check_ms) >= ENGINE_CHECK_INTERVAL_MS)
}

async fn publish_engine_launcher_blocked(blocked: bool, required: Option<&str>) {
    let _ = agent::call(
        "POST",
        "/desktop/engine-update-state",
        Some(&serde_json::json!({
            "blocked_launcher": blocked,
            "required_engine_version": required.unwrap_or_default(),
        })),
        Duration::from_secs(3),
    )
    .await;
}

pub fn engine_ready() -> bool {
    crate::runtime::binary_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

fn status_str<'a>(s: &'a Value, k: &str) -> &'a str {
    s.get(k).and_then(Value::as_str).unwrap_or("")
}

fn status_bool(s: &Value, k: &str) -> bool {
    s.get(k).and_then(Value::as_bool).unwrap_or(false)
}

// ---- startup ----

/// Build the window and its local views, then boot the agent and the hub in
/// the background. Called from the app's setup.
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let _ = APP.set(app.clone());
    let win = tauri::window::WindowBuilder::new(app, "main")
        .title("LotusProxy")
        .inner_size(1280.0, 800.0)
        .min_inner_size(1024.0, 680.0)
        .decorations(false)
        .visible(false)
        .build()?;
    let size = win.inner_size()?.to_logical::<f64>(win.scale_factor()?);
    let main =
        tauri::webview::WebviewBuilder::new("main", WebviewUrl::App("shell/index.html".into()))
            .disable_drag_drop_handler();
    win.add_child(
        main,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(size.width, size.height),
    )?;
    let drawer = tauri::webview::WebviewBuilder::new(
        "drawer",
        WebviewUrl::App("shell/index.html#drawer".into()),
    )
    .disable_drag_drop_handler();
    let d = win.add_child(
        drawer,
        LogicalPosition::new(size.width - DRAWER_W, TITLE_H),
        LogicalSize::new(DRAWER_W, size.height - TITLE_H),
    )?;
    let _ = d.hide();
    win.on_window_event(|e| match e {
        tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
            relayout()
        }
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            hide_to_tray();
        }
        _ => {}
    });
    let _ = win.show();
    tauri::async_runtime::spawn(boot());
    Ok(())
}

async fn boot() {
    if let Err(e) = agent::spawn() {
        eprintln!("[lotus] agent not started: {e:#}");
    }
    let status = agent::wait_ready(Duration::from_secs(20)).await;
    with(|sh| {
        sh.status = status.clone().unwrap_or(Value::Null);
        sh.agent_health = if status.is_some() {
            AgentHealth::Healthy
        } else {
            AgentHealth::Retrying
        };
        sh.agent_failure_since = status.is_none().then(Instant::now);
    });
    emit_local("shell://status", snapshot_value());
    tauri::async_runtime::spawn(status_loop());

    let agent_control = status
        .as_ref()
        .and_then(|s| s.get("control_url").and_then(Value::as_str));
    let saved_control = crate::settings::load()
        .ok()
        .and_then(|settings| settings.control_url);
    let control =
        resolve_control_with_setting(CONTROL_OVERRIDE, saved_control.as_deref(), agent_control);
    let Some(control) = control else {
        eprintln!("[lotus] no usable control URL");
        set_screen(Screen::Offline);
        tauri::async_runtime::spawn(check_engine_update());
        return;
    };
    with(|s| {
        s.control = Some(control.clone());
    });

    // Before any remote page: finish a logout's wipe a crash interrupted.
    let wiped = heal_hub_storage(status.as_ref()).await;
    if let Err(e) = add_hub_capability(&control) {
        eprintln!("[lotus] hub capability: {e}");
    }
    let start = if wiped {
        control.join("/login").unwrap_or(control.clone())
    } else {
        control.clone()
    };
    if let Err(e) = create_hub(start) {
        eprintln!("[lotus] hub view: {e}");
        set_screen(Screen::Offline);
    } else if !engine_ready() && status.is_some() {
        set_screen(Screen::Init);
        install_engine();
    }
    // Engine present: reconcile against the published manifest in the
    // background. The hub continues on the current engine until the swap.
    tauri::async_runtime::spawn(check_engine_update());
}

/// Wipe the hub's data folder if the persisted state says a logout did not
/// finish. True when it wiped.
async fn heal_hub_storage(status: Option<&Value>) -> bool {
    let shell = state::load();
    let auth = status.map(|s| state::AgentAuth {
        authorization: status_str(s, "authorization"),
        authorization_detail: status_str(s, "authorization_detail"),
        authorization_resolved: status_bool(s, "authorization_resolved"),
        pending_webview_clear: status_bool(s, "pending_webview_clear"),
        pending_grant: status_bool(s, "pending_grant"),
        pending_logout: status_bool(s, "pending_logout"),
    });
    if !state::must_wipe(&shell, auth.as_ref()) {
        return false;
    }
    if let Err(e) = state::wipe_hub_data() {
        // Not loading a half-signed-out view: the hub stays unbuilt.
        eprintln!("[lotus] hub storage wipe failed: {e:#}");
        return false;
    }
    let agent_waiting = status.is_some_and(|s| {
        status_bool(s, "pending_webview_clear") && status_str(s, "logout_stage") == "webview_clear"
    });
    if agent_waiting {
        let _ = agent::call(
            "POST",
            "/desktop/logout/webview-cleared",
            None,
            Duration::from_secs(3),
        )
        .await;
    }
    let _ = state::update(|s| {
        s.pending_webview_clear = false;
        s.bound = false;
    });
    true
}

/// The hub's runtime capability: its `desktop_*` commands, from the control
/// origin only, in the hub view only.
fn add_hub_capability(control: &url::Url) -> tauri::Result<()> {
    let Some(app) = app() else { return Ok(()) };
    let mut cap = tauri::ipc::CapabilityBuilder::new("hub")
        .remote(format!("{}/*", control.origin().ascii_serialization()))
        .webview("hub");
    for c in commands::HUB_COMMANDS {
        cap = cap.permission(format!("allow-{}", c.replace('_', "-")));
    }
    app.add_capability(cap)
}

fn create_hub(start: url::Url) -> tauri::Result<()> {
    let Some(app) = app() else { return Ok(()) };
    let Some(win) = app.get_window("main") else {
        return Ok(());
    };
    let dir = state::hub_data_dir().map_err(|e| tauri::Error::Anyhow(e))?;
    let _ = std::fs::create_dir_all(&dir);
    let origin = start.origin().ascii_serialization();
    let script = include_str!("bridge.js").replace(
        "__LOTUS_CONTROL_ORIGIN__",
        &serde_json::to_string(&origin).unwrap_or_default(),
    );
    let hub = tauri::webview::WebviewBuilder::new("hub", WebviewUrl::External(start))
        .data_directory(dir)
        .initialization_script(script)
        .on_navigation(|url| guard_navigation(url))
        .on_new_window(|url, _| {
            if !is_control(&url) {
                open_external(&url);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_page_load(|_, p| {
            if matches!(p.event(), tauri::webview::PageLoadEvent::Started) {
                hub_loading();
            }
        });
    let hub = win.add_child(
        hub,
        LogicalPosition::new(0.0, TITLE_H),
        LogicalSize::new(1.0, 1.0),
    )?;
    let _ = hub.hide();
    hub_loading();
    Ok(())
}

/// Only the control origin loads in the hub view. Other web links go to the
/// system browser; anything else (file:, javascript:, custom schemes) nowhere.
fn guard_navigation(url: &url::Url) -> bool {
    if is_control(url) {
        return true;
    }
    if url.scheme() == "about" && url.path() == "blank" {
        return true;
    }
    open_external(url);
    false
}

fn open_external(url: &url::Url) {
    if matches!(url.scheme(), "https" | "http" | "mailto") {
        let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
    }
}

/// A hub page started loading: it has `HUB_READY_TIMEOUT` to say it is up.
fn hub_loading() {
    let round = with(|s| {
        s.hub_ready = false;
        s.hub_round += 1;
        s.hub_offline_round += 1;
        s.hub_round
    });
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(HUB_READY_TIMEOUT).await;
        // The engine's preparation page stays up; it moves on once both are ready.
        let late = with(|s| {
            s.hub_round == round
                && !s.hub_ready
                && matches!(s.screen, Screen::Starting | Screen::Daily)
        });
        if late {
            set_screen(Screen::Offline);
        }
    });
}

/// The bridge on a control-origin page said it loaded.
pub fn hub_ready() {
    let (healthy, screen) = with(|s| {
        s.hub_ready = true;
        s.hub_offline_round += 1;
        (s.agent_health == AgentHealth::Healthy, s.screen)
    });
    if healthy && matches!(screen, Screen::Starting | Screen::Offline) {
        set_screen(if engine_ready() {
            Screen::Daily
        } else {
            Screen::Init
        });
    }
}

pub fn hub_offline() {
    let round = with(|s| {
        s.hub_ready = false;
        s.hub_offline_round += 1;
        s.hub_offline_round
    });
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(HUB_OFFLINE_GRACE).await;
        let (current, ready) = with(|s| (s.hub_offline_round, s.hub_ready));
        let expired = offline_timer_is_current(round, current, ready);
        if expired {
            set_screen(Screen::Offline);
        }
    });
}

fn offline_timer_is_current(round: u64, current: u64, hub_ready: bool) -> bool {
    round == current && !hub_ready
}

/// Load the control origin again (offline page's "reconnect").
pub fn retry_hub() {
    let Some(app) = app() else { return };
    let Some(control) = with(|s| s.control.clone()) else {
        return;
    };
    match app.get_webview("hub") {
        Some(hub) => {
            hub_loading();
            let _ = hub.navigate(control);
        }
        None => {
            if create_hub(control).is_err() {
                set_screen(Screen::Offline);
            }
        }
    }
}

// ---- the agent's status, and the last stage of a logout ----

async fn status_loop() {
    let mut misses = 0u32;
    let mut restart_at = 3u32;
    let mut restart_delay = 3u32;
    loop {
        if QUITTING.load(SeqCst) {
            return;
        }
        match agent::status().await {
            Ok(s) => {
                misses = 0;
                restart_at = 3;
                restart_delay = 3;
                on_status(s).await;
            }
            Err(_) => {
                misses += 1;
                // The agent died under the shell: retry bounded restarts until
                // its status endpoint is reachable again.
                if misses >= restart_at && agent::exited() {
                    let _ = agent::spawn();
                    restart_at = misses.saturating_add(restart_delay);
                    restart_delay = (restart_delay * 2).min(60);
                }
                let changed = with(|sh| {
                    let since = *sh.agent_failure_since.get_or_insert_with(Instant::now);
                    let health = agent_health_after_failure(since.elapsed());
                    let changed = sh.agent_health != health;
                    sh.agent_health = health;
                    changed
                });
                if changed {
                    emit_local("shell://status", snapshot_value());
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn on_status(s: Value) {
    let (changed, healthy_transition, hub_ready, screen) = with(|sh| {
        let c = sh.status != s || sh.agent_health != AgentHealth::Healthy;
        let was_healthy = sh.agent_health == AgentHealth::Healthy;
        sh.status = s.clone();
        sh.agent_health = AgentHealth::Healthy;
        sh.agent_failure_since = None;
        (c, !was_healthy, sh.hub_ready, sh.screen)
    });
    if changed {
        emit_local("shell://status", snapshot_value());
        let _ = update_tray();
    }
    schedule_engine_update_for_status(&s);
    if status_str(&s, "authorization") == "authorized" && !state::load().bound {
        let _ = state::update(|st| st.bound = true);
    }
    if healthy_transition {
        if !engine_ready() {
            set_screen(Screen::Init);
            install_engine();
        } else if hub_ready && matches!(screen, Screen::Starting | Screen::Offline) {
            set_screen(Screen::Daily);
        }
    }
    let waiting = status_bool(&s, "pending_webview_clear")
        && status_str(&s, "logout_stage") == "webview_clear";
    if !waiting {
        with(|sh| {
            sh.logout_seen = false;
            sh.hub_logout_ack = false;
        });
        return;
    }
    let (first, ack, since) = with(|sh| {
        let first = !sh.logout_seen;
        if first {
            sh.logout_seen = true;
            sh.logout_requested.get_or_insert_with(Instant::now);
        }
        (
            first,
            sh.hub_logout_ack,
            sh.logout_requested.unwrap_or_else(Instant::now),
        )
    });
    if first {
        // The latch, in case this logout started from the tray or the agent.
        let _ = state::update(|st| st.pending_webview_clear = true);
    }
    if ack || since.elapsed() >= LOGOUT_ACK_WAIT || !with(|sh| sh.hub_ready) {
        finish_logout_wipe().await;
    }
}

fn schedule_engine_update_for_status(status: &Value) {
    let session_active = status_bool(status, "session_active");
    let retry_busy = !session_active
        && ENGINE_BUSY_RETRY_PENDING.swap(false, SeqCst)
        && !ENGINE_BUSY_RETRY_USED.swap(true, SeqCst);
    if retry_busy {
        tauri::async_runtime::spawn(check_engine_update_inner(true, true, false));
        return;
    }

    let hint = status_str(status, "engine_update_hint");
    if hint.is_empty()
        || crate::runtime::engine_version()
            .as_deref()
            .and_then(|installed| crate::runtime::engine_version_satisfies(installed, &hint))
            == Some(true)
    {
        return;
    }
    let should_check = with(|shell| {
        let ready = shell
            .last_engine_hint_check
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5 * 60));
        if ready {
            shell.last_engine_hint_check = Some(Instant::now());
            shell.engine_required_version = Some(hint.to_owned());
        }
        ready
    });
    if should_check {
        tauri::async_runtime::spawn(check_engine_update_inner(true, false, false));
    }
}

fn agent_health_after_failure(elapsed: Duration) -> AgentHealth {
    if elapsed >= AGENT_STATUS_GRACE {
        AgentHealth::Offline
    } else {
        AgentHealth::Retrying
    }
}

/// The hub said its IAM session ended (or tried to for 3 s).
pub fn hub_logout_ack() {
    with(|s| s.hub_logout_ack = true);
}

/// The shell latched a logout it asked the agent for.
pub fn logout_requested() {
    with(|s| {
        s.logout_requested = Some(Instant::now());
        s.hub_logout_ack = false;
    });
}

pub fn logout_cancelled() {
    with(|s| {
        s.logout_requested = None;
        s.logout_seen = false;
        s.hub_logout_ack = false;
    });
}

/// Wipe the live hub view's storage, tell the agent, land on the login page.
/// A failure keeps the latch: the next start wipes the folder before loading.
async fn finish_logout_wipe() {
    if WIPING.swap(true, SeqCst) {
        return;
    }
    let ok = match app().and_then(|a| a.get_webview("hub")) {
        Some(hub) => {
            let _ = hub.navigate("about:blank".parse().expect("static url"));
            hub.clear_all_browsing_data().is_ok()
        }
        // No hub view: nothing in memory; the folder itself goes.
        None => state::wipe_hub_data().is_ok(),
    };
    if ok {
        let told = agent::call(
            "POST",
            "/desktop/logout/webview-cleared",
            None,
            Duration::from_secs(3),
        )
        .await;
        if told.is_ok() {
            let _ = state::update(|s| {
                s.pending_webview_clear = false;
                s.bound = false;
            });
            logout_cancelled();
            with(|s| s.logout_requested = None);
            if let (Some(hub), Some(control)) = (
                app().and_then(|a| a.get_webview("hub")),
                with(|s| s.control.clone()),
            ) {
                hub_loading();
                let _ = hub.navigate(control.join("/login").unwrap_or(control));
            }
        }
    } else {
        eprintln!("[lotus] hub storage wipe failed; retried at the next start");
    }
    WIPING.store(false, SeqCst);
}

// ---- tray, close, quit ----

fn hide_to_tray() {
    let Some(app) = app() else { return };
    if let Some(w) = app.get_window("main") {
        let _ = w.hide();
    }
    if !state::load().tray_hint_shown {
        let _ = state::update(|s| s.tray_hint_shown = true);
        tray_notice(
            app,
            "LotusProxy 仍在托盘运行",
            "点托盘图标可重新打开；要退出请在托盘菜单选「退出」。",
        );
    }
}

fn tray_notice(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(title).body(body).show();
}

pub fn show_window() {
    let Some(app) = app() else { return };
    if let Some(w) = app.get_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn show_window_from_tray() {
    show_window();
    if should_check_engine_from_tray() {
        tauri::async_runtime::spawn(check_engine_update());
    }
}

pub fn open_settings() {
    show_window();
    let screen = with(|s| s.screen);
    if screen == Screen::Daily {
        set_drawer(true);
    } else {
        // The full-window pages show the same cards themselves.
        emit_local("shell://open-settings", Value::Null);
    }
}

const TRAY_ID: &str = "lotus";

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let show = MenuItem::with_id(app, "tray_show", "打开主界面", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "tray_stop", "停止当前会话", false, None::<&str>)?;
    let settings = MenuItem::with_id(app, "tray_settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray_quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &stop, &settings, &sep, &quit])?;
    let mut b = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("LotusProxy")
        .menu(&menu)
        .show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon().cloned() {
        b = b.icon(icon);
    }
    b.on_menu_event(|_, e| match e.id.as_ref() {
        "tray_show" => show_window_from_tray(),
        "tray_stop" => {
            tauri::async_runtime::spawn(async {
                let _ = agent::call("POST", "/desktop/stop", None, Duration::from_secs(100)).await;
            });
        }
        "tray_settings" => open_settings(),
        "tray_quit" => self::quit(),
        _ => {}
    })
    .on_tray_icon_event(|_, e| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            show_window_from_tray();
        }
    })
    .build(app)?;
    TRAY_STOP.with_item(stop);
    Ok(())
}

/// The tray's stop item, enabled only while a session runs.
struct TrayStop(Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>);
impl TrayStop {
    fn with_item(&self, item: tauri::menu::MenuItem<tauri::Wry>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(item);
    }
}
static TRAY_STOP: TrayStop = TrayStop(Mutex::new(None));

fn update_tray() -> tauri::Result<()> {
    let active = with(|s| status_bool(&s.status, "session_active"));
    if let Some(item) = TRAY_STOP
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        item.set_enabled(active)?;
    }
    Ok(())
}

/// Quit: the quitting page, the agent's own cleanup (its 3 min budget), then
/// exit. Past `QUIT_HARD_CAP` the shell goes regardless; the agent keeps what
/// it owes on disk and settles it at its next start.
pub fn quit() {
    if QUITTING.swap(true, SeqCst) {
        show_window();
        return;
    }
    show_window();
    with(|s| s.drawer = false);
    with(|s| s.screen = Screen::Quitting);
    relayout();
    tauri::async_runtime::spawn(async {
        let _ = agent::call("POST", "/desktop/quit", None, Duration::from_secs(5)).await;
        let until = Instant::now() + QUIT_HARD_CAP;
        loop {
            let gone = agent::exited() && agent::status().await.is_err();
            if gone || Instant::now() >= until {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        agent::kill();
        QUIT_DONE.store(true, SeqCst);
        if let Some(app) = app() {
            app.exit(0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_puts_the_hub_under_the_title_bar() {
        let l = layout(Screen::Daily, false, 1280.0, 800.0);
        assert_eq!(
            l.main,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1280.0,
                h: TITLE_H
            }
        );
        assert_eq!(
            l.hub,
            Some(Rect {
                x: 0.0,
                y: TITLE_H,
                w: 1280.0,
                h: 800.0 - TITLE_H
            })
        );
        assert_eq!(l.drawer, None);
    }

    #[test]
    fn daily_engine_banner_reserves_space_above_the_hub() {
        let l = layout_with_engine_banner(Screen::Daily, false, 1280.0, 800.0, true);
        assert_eq!(l.main.h, TITLE_H + ENGINE_BANNER_H);
        assert_eq!(l.hub.as_ref().unwrap().y, TITLE_H + ENGINE_BANNER_H);
        assert_eq!(l.hub.as_ref().unwrap().h, 800.0 - TITLE_H - ENGINE_BANNER_H);
    }

    #[test]
    fn the_drawer_shrinks_the_hub_and_never_covers_it() {
        let l = layout(Screen::Daily, true, 1280.0, 800.0);
        let hub = l.hub.unwrap();
        let drawer = l.drawer.unwrap();
        assert_eq!(hub.w, 1280.0 - DRAWER_W);
        assert_eq!(
            drawer,
            Rect {
                x: 1280.0 - DRAWER_W,
                y: TITLE_H,
                w: DRAWER_W,
                h: 800.0 - TITLE_H
            }
        );
        assert_eq!(hub.x + hub.w, drawer.x);
    }

    #[test]
    fn full_window_screens_hide_the_hub() {
        for s in [
            Screen::Starting,
            Screen::Init,
            Screen::Offline,
            Screen::Quitting,
        ] {
            let l = layout(s, true, 1280.0, 800.0);
            assert_eq!(
                l.main,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1280.0,
                    h: 800.0
                }
            );
            assert!(l.hub.is_none() && l.drawer.is_none(), "{s:?}");
        }
    }

    #[test]
    fn an_old_offline_timer_cannot_override_recovery() {
        assert!(offline_timer_is_current(4, 4, false));
        assert!(!offline_timer_is_current(4, 5, false));
        assert!(!offline_timer_is_current(4, 4, true));
    }

    #[test]
    fn an_agent_timeout_is_retrying_until_its_grace_expires() {
        assert_eq!(
            agent_health_after_failure(Duration::from_secs(14)),
            AgentHealth::Retrying
        );
        assert_eq!(
            agent_health_after_failure(AGENT_STATUS_GRACE),
            AgentHealth::Offline
        );
    }

    #[test]
    fn only_an_https_origin_is_a_control_url() {
        assert_eq!(
            parse_control("https://hub.lotusrain.net/x")
                .unwrap()
                .as_str(),
            "https://hub.lotusrain.net/"
        );
        for bad in [
            "http://hub.lotusrain.net",
            "https://u:p@hub.lotusrain.net",
            "https://hub.lotusrain.net/?a=1",
            "file:///etc",
            "javascript:alert(1)",
        ] {
            assert!(parse_control(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn explicit_local_control_override_wins_over_the_agent_endpoint() {
        let control = resolve_control(
            Some("http://localhost:3000/path"),
            Some("https://hub.lotusrain.net"),
        );
        assert_eq!(control.unwrap().as_str(), "http://localhost:3000/");
    }

    #[test]
    fn a_saved_local_control_url_overrides_the_agent_endpoint() {
        let control = resolve_control_with_setting(
            None,
            Some("http://127.0.0.1:8080"),
            Some("https://hub.lotusrain.net"),
        );
        assert_eq!(control.unwrap().as_str(), "http://127.0.0.1:8080/");
    }

    #[test]
    fn invalid_saved_control_url_fails_closed() {
        assert!(resolve_control_with_setting(
            None,
            Some("http://hub.lotusrain.net"),
            Some("https://hub.lotusrain.net"),
        )
        .is_none());
    }

    #[test]
    fn an_invalid_control_override_does_not_fall_back_to_production() {
        assert!(resolve_control(
            Some("http://hub.lotusrain.net"),
            Some("https://hub.lotusrain.net")
        )
        .is_none());
    }

    #[test]
    fn absent_control_override_keeps_the_agent_endpoint() {
        assert_eq!(
            resolve_control(None, Some("https://control.example.test/path"))
                .unwrap()
                .as_str(),
            "https://control.example.test/"
        );
    }
}
