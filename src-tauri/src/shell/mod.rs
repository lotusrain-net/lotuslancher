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
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl};

pub use commands::*;

pub const TITLE_H: f64 = 36.0;
pub const DRAWER_W: f64 = 360.0;
/// How long a hub page may take to say it loaded before the offline screen.
const HUB_READY_TIMEOUT: Duration = Duration::from_secs(12);
/// Past the agent's own 3 min quit budget, the shell goes regardless.
const QUIT_HARD_CAP: Duration = Duration::from_secs(195);
/// How long the hub gets to end its IAM session before the wipe goes ahead.
const LOGOUT_ACK_WAIT: Duration = Duration::from_secs(5);
const DEFAULT_CONTROL: &str = match option_env!("LOTUS_CONTROL_URL") {
    Some(u) => u,
    None => "https://hub.lotusrain.net",
};

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

struct Shell {
    screen: Screen,
    drawer: bool,
    control: Option<url::Url>,
    hub_ready: bool,
    /// Bumped on every hub navigation, so a late timer of an earlier one is ignored.
    hub_round: u64,
    status: Value,
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
    status: Value::Null,
    logout_requested: None,
    logout_seen: false,
    hub_logout_ack: false,
});
static APP: OnceLock<AppHandle> = OnceLock::new();
static WIPING: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);
static QUIT_DONE: AtomicBool = AtomicBool::new(false);

fn with<T>(f: impl FnOnce(&mut Shell) -> T) -> T {
    let mut g = SHELL.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

fn app() -> Option<&'static AppHandle> {
    APP.get()
}

/// A URL is the control origin's own: same scheme, host and port.
pub fn is_control(url: &url::Url) -> bool {
    with(|s| s.control.as_ref().is_some_and(|c| c.origin() == url.origin()))
}

/// Accept the agent's control URL: https, or http on loopback for development.
fn parse_control(raw: &str) -> Option<url::Url> {
    let u = url::Url::parse(raw).ok()?;
    let host = u.host_str()?;
    let loopback = host == "127.0.0.1" || host == "localhost" || host == "[::1]";
    let ok = (u.scheme() == "https" || (u.scheme() == "http" && loopback && cfg!(debug_assertions)))
        && u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none();
    ok.then(|| url::Url::parse(&u.origin().ascii_serialization()).ok()).flatten()
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
    let body = (h - TITLE_H).max(0.0);
    match screen {
        Screen::Daily => {
            let drawer_w = if drawer { DRAWER_W.min(w) } else { 0.0 };
            Layout {
                main: Rect { x: 0.0, y: 0.0, w, h: TITLE_H.min(h) },
                drawer: drawer.then(|| Rect { x: w - drawer_w, y: TITLE_H, w: drawer_w, h: body }),
                hub: Some(Rect { x: 0.0, y: TITLE_H, w: (w - drawer_w).max(0.0), h: body }),
            }
        }
        // The full-window pages draw their own drawer when asked for one.
        _ => Layout { main: Rect { x: 0.0, y: 0.0, w, h }, drawer: None, hub: None },
    }
}

fn place(v: &Webview, r: &Rect) {
    let _ = v.set_position(LogicalPosition::new(r.x, r.y));
    let _ = v.set_size(LogicalSize::new(r.w, r.h));
}

/// Put every view where the current screen wants it and tell the local views.
pub fn relayout() {
    let Some(app) = app() else { return };
    let Some(win) = app.get_window("main") else { return };
    let (screen, drawer) = with(|s| (s.screen, s.drawer));
    let scale = win.scale_factor().unwrap_or(1.0);
    let Ok(size) = win.inner_size() else { return };
    let size = size.to_logical::<f64>(scale);
    let l = layout(screen, drawer, size.width, size.height);
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

pub fn set_drawer(open: bool) {
    with(|s| s.drawer = open);
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
    let (screen, drawer, status, control) = with(|s| (s.screen, s.drawer, s.status.clone(), s.control.clone()));
    serde_json::json!({
        "screen": screen,
        "drawer": drawer,
        "status": status,
        "engine_ready": engine_ready(),
        "engine_installing": INSTALLING.load(SeqCst),
        "engine_error": ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "shell_version": env!("CARGO_PKG_VERSION"),
        "control_host": control.as_ref().and_then(|u| u.host_str().map(str::to_owned)),
        "autostart_available": autostart::AVAILABLE,
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
        "driver_ready": pick("driver_ready"),
        "engine_ready": engine_ready(),
        "session_active": pick("session_active"),
        "cleanup_pending": pick("cleanup_pending"),
        "cleanup_phase": pick("cleanup_phase"),
        "pending_logout": pick("pending_logout"),
        "logout_stage": pick("logout_stage"),
        "pending_grant": pick("pending_grant"),
        "error": s.get("error").and_then(Value::as_str).map(agent::sanitize_code).filter(|c| c != "operation_failed"),
    })
}

static INSTALLING: AtomicBool = AtomicBool::new(false);
static ENGINE_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Download the browser engine (first run, or the init page's retry). Its
/// progress reaches the local views as `runtime:progress`.
pub fn install_engine() {
    if engine_ready() || INSTALLING.swap(true, SeqCst) {
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
                let _ = agent::call("POST", "/desktop/action", Some(&serde_json::json!({ "name": "probe" })), Duration::from_secs(100)).await;
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
                *ENGINE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some("engine_install_failed".into());
            }
        }
        emit_local("shell://status", snapshot_value());
    });
}

pub fn engine_ready() -> bool {
    crate::runtime::binary_path().map(|p| p.exists()).unwrap_or(false)
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
    let main = tauri::webview::WebviewBuilder::new("main", WebviewUrl::App("shell/index.html".into())).disable_drag_drop_handler();
    win.add_child(main, LogicalPosition::new(0.0, 0.0), LogicalSize::new(size.width, size.height))?;
    let drawer = tauri::webview::WebviewBuilder::new("drawer", WebviewUrl::App("shell/index.html#drawer".into())).disable_drag_drop_handler();
    let d = win.add_child(drawer, LogicalPosition::new(size.width - DRAWER_W, TITLE_H), LogicalSize::new(DRAWER_W, size.height - TITLE_H))?;
    let _ = d.hide();
    win.on_window_event(|e| match e {
        tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => relayout(),
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
    let control = status
        .as_ref()
        .and_then(|s| s.get("control_url").and_then(Value::as_str))
        .and_then(parse_control)
        .or_else(|| parse_control(DEFAULT_CONTROL));
    let Some(control) = control else {
        eprintln!("[lotus] no usable control URL");
        set_screen(Screen::Offline);
        return;
    };
    with(|s| {
        s.control = Some(control.clone());
        s.status = status.clone().unwrap_or(Value::Null);
    });

    // Before any remote page: finish a logout's wipe a crash interrupted.
    let wiped = heal_hub_storage(status.as_ref()).await;
    if let Err(e) = add_hub_capability(&control) {
        eprintln!("[lotus] hub capability: {e}");
    }
    let start = if wiped { control.join("/login").unwrap_or(control.clone()) } else { control.clone() };
    if let Err(e) = create_hub(start) {
        eprintln!("[lotus] hub view: {e}");
        set_screen(Screen::Offline);
    } else if !engine_ready() {
        set_screen(Screen::Init);
        install_engine();
    }
    tauri::async_runtime::spawn(status_loop());
}

/// Wipe the hub's data folder if the persisted state says a logout did not
/// finish. True when it wiped.
async fn heal_hub_storage(status: Option<&Value>) -> bool {
    let shell = state::load();
    let auth = status.map(|s| state::AgentAuth {
        authorization: status_str(s, "authorization"),
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
    let agent_waiting = status.is_some_and(|s| status_bool(s, "pending_webview_clear") && status_str(s, "logout_stage") == "webview_clear");
    if agent_waiting {
        let _ = agent::call("POST", "/desktop/logout/webview-cleared", None, Duration::from_secs(3)).await;
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
    let mut cap = tauri::ipc::CapabilityBuilder::new("hub").remote(format!("{}/*", control.origin().ascii_serialization())).webview("hub");
    for c in commands::HUB_COMMANDS {
        cap = cap.permission(format!("allow-{}", c.replace('_', "-")));
    }
    app.add_capability(cap)
}

fn create_hub(start: url::Url) -> tauri::Result<()> {
    let Some(app) = app() else { return Ok(()) };
    let Some(win) = app.get_window("main") else { return Ok(()) };
    let dir = state::hub_data_dir().map_err(|e| tauri::Error::Anyhow(e))?;
    let _ = std::fs::create_dir_all(&dir);
    let origin = start.origin().ascii_serialization();
    let script = include_str!("bridge.js").replace("__LOTUS_CONTROL_ORIGIN__", &serde_json::to_string(&origin).unwrap_or_default());
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
    let hub = win.add_child(hub, LogicalPosition::new(0.0, TITLE_H), LogicalSize::new(1.0, 1.0))?;
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
        s.hub_round
    });
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(HUB_READY_TIMEOUT).await;
        // The engine's preparation page stays up; it moves on once both are ready.
        let late = with(|s| s.hub_round == round && !s.hub_ready && matches!(s.screen, Screen::Starting | Screen::Daily));
        if late {
            set_screen(Screen::Offline);
        }
    });
}

/// The bridge on a control-origin page said it loaded.
pub fn hub_ready() {
    with(|s| s.hub_ready = true);
    let screen = with(|s| s.screen);
    if matches!(screen, Screen::Starting | Screen::Offline) {
        set_screen(if engine_ready() { Screen::Daily } else { Screen::Init });
    }
}

pub fn hub_offline() {
    set_screen(Screen::Offline);
}

/// Load the control origin again (offline page's "reconnect").
pub fn retry_hub() {
    let Some(app) = app() else { return };
    let Some(control) = with(|s| s.control.clone()) else { return };
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
    loop {
        if QUITTING.load(SeqCst) {
            return;
        }
        match agent::status().await {
            Ok(s) => {
                misses = 0;
                on_status(s).await;
            }
            Err(_) => {
                misses += 1;
                // The agent died under the shell: start it again, quietly.
                if misses == 3 && agent::exited() {
                    let _ = agent::spawn();
                }
                if misses == 3 {
                    with(|s| s.status = Value::Null);
                    emit_local("shell://status", snapshot_value());
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn on_status(s: Value) {
    let changed = with(|sh| {
        let c = sh.status != s;
        sh.status = s.clone();
        c
    });
    if changed {
        emit_local("shell://status", snapshot_value());
        let _ = update_tray();
    }
    if status_str(&s, "authorization") == "authorized" && !state::load().bound {
        let _ = state::update(|st| st.bound = true);
    }
    let waiting = status_bool(&s, "pending_webview_clear") && status_str(&s, "logout_stage") == "webview_clear";
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
        (first, sh.hub_logout_ack, sh.logout_requested.unwrap_or_else(Instant::now))
    });
    if first {
        // The latch, in case this logout started from the tray or the agent.
        let _ = state::update(|st| st.pending_webview_clear = true);
    }
    if ack || since.elapsed() >= LOGOUT_ACK_WAIT || !with(|sh| sh.hub_ready) {
        finish_logout_wipe().await;
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
        let told = agent::call("POST", "/desktop/logout/webview-cleared", None, Duration::from_secs(3)).await;
        if told.is_ok() {
            let _ = state::update(|s| {
                s.pending_webview_clear = false;
                s.bound = false;
            });
            logout_cancelled();
            with(|s| s.logout_requested = None);
            if let (Some(hub), Some(control)) = (app().and_then(|a| a.get_webview("hub")), with(|s| s.control.clone())) {
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
        tray_notice(app, "LotusProxy 仍在托盘运行", "点托盘图标可重新打开；要退出请在托盘菜单选「退出」。");
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
    let mut b = TrayIconBuilder::with_id(TRAY_ID).tooltip("LotusProxy").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon().cloned() {
        b = b.icon(icon);
    }
    b.on_menu_event(|_, e| match e.id.as_ref() {
        "tray_show" => show_window(),
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
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            show_window();
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
    if let Some(item) = TRAY_STOP.0.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
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
        assert_eq!(l.main, Rect { x: 0.0, y: 0.0, w: 1280.0, h: TITLE_H });
        assert_eq!(l.hub, Some(Rect { x: 0.0, y: TITLE_H, w: 1280.0, h: 800.0 - TITLE_H }));
        assert_eq!(l.drawer, None);
    }

    #[test]
    fn the_drawer_shrinks_the_hub_and_never_covers_it() {
        let l = layout(Screen::Daily, true, 1280.0, 800.0);
        let hub = l.hub.unwrap();
        let drawer = l.drawer.unwrap();
        assert_eq!(hub.w, 1280.0 - DRAWER_W);
        assert_eq!(drawer, Rect { x: 1280.0 - DRAWER_W, y: TITLE_H, w: DRAWER_W, h: 800.0 - TITLE_H });
        assert_eq!(hub.x + hub.w, drawer.x);
    }

    #[test]
    fn full_window_screens_hide_the_hub() {
        for s in [Screen::Starting, Screen::Init, Screen::Offline, Screen::Quitting] {
            let l = layout(s, true, 1280.0, 800.0);
            assert_eq!(l.main, Rect { x: 0.0, y: 0.0, w: 1280.0, h: 800.0 });
            assert!(l.hub.is_none() && l.drawer.is_none(), "{s:?}");
        }
    }

    #[test]
    fn only_an_https_origin_is_a_control_url() {
        assert_eq!(parse_control("https://hub.lotusrain.net/x").unwrap().as_str(), "https://hub.lotusrain.net/");
        for bad in ["http://hub.lotusrain.net", "https://u:p@hub.lotusrain.net", "https://hub.lotusrain.net/?a=1", "file:///etc", "javascript:alert(1)"] {
            assert!(parse_control(bad).is_none(), "{bad}");
        }
    }
}
