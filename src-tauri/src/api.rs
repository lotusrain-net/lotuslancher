// Local driver HTTP API (axum) for the LotusProxy desktop shell.
//
// Only the embedded Go agent talks to it, over plain net/http on
// 127.0.0.1:40325. The shell also runs a remote web page and the user's
// browsers can reach loopback, so every request passes `guard` first:
//   * Host must be exactly 127.0.0.1:40325 (DNS rebinding);
//   * any browser-only header (Origin, Referer, Sec-Fetch-*, preflight
//     Access-Control-Request-*) and every OPTIONS is refused with 403;
//   * no CORS or Private Network Access header is ever sent back.
// Every route but GET /health then needs a short-lived HS256 JWT (≤60 s,
// ±30 s clock skew), and the route set is the agent's session lifecycle only.

use std::sync::{OnceLock, RwLock};

use axum::{
    extract::{Path, Request},
    http::{header::AUTHORIZATION, header::HOST, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::space::{self, CmdError, Door, Ticket};

/// The one address the driver API listens on and answers for.
pub const API_PORT: u16 = 40325;
pub const API_HOST: &str = "127.0.0.1:40325";
/// What `/health` reports. Fixed rather than the product version, so the
/// agent's driver pin does not move with every LotusProxy release.
pub const DRIVER_PROTOCOL_VERSION: &str = "2.1.6";
/// Shortest signing secret the server accepts, in bytes.
pub const MIN_SECRET_LEN: usize = 32;
/// Longest lifetime (exp - iat) a token may claim.
const TOKEN_MAX_TTL: u64 = 60;
/// Clock skew allowed on iat and exp.
const CLOCK_SKEW: u64 = 30;

// ---- HS256 secret (process-global so live rotation invalidates old tokens) ----

fn secret_cell() -> &'static RwLock<String> {
    static SECRET: OnceLock<RwLock<String>> = OnceLock::new();
    SECRET.get_or_init(|| RwLock::new(String::new()))
}

/// Install/replace the signing secret.
pub fn set_secret(s: &str) {
    if let Ok(mut g) = secret_cell().write() {
        *g = s.to_string();
    }
}

fn read_secret() -> String {
    secret_cell().read().map(|g| g.clone()).unwrap_or_default()
}

// ---- JWT ----

#[derive(serde::Serialize, serde::Deserialize)]
struct Claims {
    sub: String,
    iat: u64,
    exp: u64,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
fn mint_at(secret: &str, iat: u64, exp: u64) -> String {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    let claims = Claims {
        sub: "shardx-api".into(),
        iat,
        exp,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

/// A token the agent would send: issued now, valid for `ttl_secs`.
#[cfg(test)]
fn mint(secret: &str, ttl_secs: u64) -> String {
    let now = unix_now();
    mint_at(secret, now, now + ttl_secs)
}

fn verify(secret: &str, token: &str) -> bool {
    verify_at(secret, token, unix_now())
}

fn verify_at(secret: &str, token: &str, now: u64) -> bool {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
    if secret.len() < MIN_SECRET_LEN {
        return false;
    }
    let mut v = Validation::new(Algorithm::HS256);
    // exp is checked below against our own clock, with the skew.
    v.validate_exp = false;
    v.set_required_spec_claims(&["exp", "iat", "sub"]);
    let Ok(data) = decode::<Claims>(token, &DecodingKey::from_secret(secret.as_bytes()), &v) else {
        return false;
    };
    let c = data.claims;
    c.sub == "shardx-api"
        && c.exp > c.iat
        && c.exp - c.iat <= TOKEN_MAX_TTL
        && c.iat <= now.saturating_add(CLOCK_SKEW)
        && c.exp.saturating_add(CLOCK_SKEW) > now
}
// ---- error type ----

struct ApiError(StatusCode, Value);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, json!({ "error": msg.into() }))
}

/// A door's refusal: invisible 404, refused 403 with what it needs, a
/// conflict 409, the team server unreachable 503.
fn refusal(c: &CmdError) -> ApiError {
    let mut body = json!({ "error": c.to_string(), "code": c.code() });
    match c {
        CmdError::Forbidden(n) => {
            body["need"] = json!(n.right);
            body["on"] = json!(n.on);
        }
        CmdError::Stale { now: Some(n), .. } => body["current_revision"] = json!(n),
        CmdError::Leased(device) => body["device_name"] = json!(device),
        _ => {}
    }
    ApiError(
        StatusCode::from_u16(c.status()).unwrap_or(StatusCode::BAD_GATEWAY),
        body,
    )
}

/// The refusal inside an error, or `fallback` with its words.
fn fail<E: Into<anyhow::Error>>(fallback: StatusCode) -> impl Fn(E) -> ApiError {
    move |e| {
        let e = e.into();
        match space::error::find(&e) {
            Some(c) => refusal(c),
            None => err(fallback, e.to_string()),
        }
    }
}

// ---- the space a request is about ----

/// Sent back on every answer: the space's edition the answer is about. A caller
/// that sends it on a write gets 409 space_changed once the launcher has moved on.
const EPOCH_HEADER: &str = "x-space-epoch";

/// Every request gets the ticket of the space in use as it came in; a write
/// goes to that space or nowhere.
async fn in_space(mut req: Request, next: Next) -> Response {
    let asked = match req.headers().get(EPOCH_HEADER) {
        None => None,
        Some(v) => match v.to_str().ok().and_then(|s| s.trim().parse::<u64>().ok()) {
            Some(n) => Some(n),
            None => {
                return err(StatusCode::BAD_REQUEST, "X-Space-Epoch is a number").into_response()
            }
        },
    };
    let t = match space::ticket(asked) {
        Ok(t) => t,
        Err(e) => return refusal(&e).into_response(),
    };
    let epoch = t.epoch();
    req.extensions_mut().insert(t);
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(EPOCH_HEADER, HeaderValue::from(epoch));
    res
}

type ApiResult = Result<Json<Value>, ApiError>;

// ---- auth middleware ----

async fn auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let secret = read_secret();
    let ok = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.strip_prefix("Bearer ")
                .or_else(|| h.strip_prefix("bearer "))
        })
        .map(|t| verify(&secret, t.trim()))
        .unwrap_or(false);
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

// ---- handlers ----

// ---- browser and DNS-rebinding guard (outermost, every route) ----

/// Headers only a browser sends. The agent's net/http client sends none, so
/// one of these means a web page (the shell's own remote view included) is
/// reaching for the driver.
fn browser_marked(req: &Request) -> bool {
    let h = req.headers();
    h.contains_key("origin")
        || h.contains_key("referer")
        || h.keys().any(|k| {
            let k = k.as_str();
            k.starts_with("sec-fetch-") || k.starts_with("access-control-request-")
        })
}

/// The request names this server, and only this server, as its host.
fn host_is_ours(req: &Request) -> bool {
    // Exactly one Host header, byte for byte ours.
    let mut hosts = req.headers().get_all(HOST).iter();
    let header_ok = matches!((hosts.next(), hosts.next()), (Some(v), None) if v.as_bytes() == API_HOST.as_bytes());
    // An absolute-form target (`GET http://evil/ …`) carries its own authority.
    let target_ok = req.uri().authority().is_none_or(|a| a.as_str() == API_HOST);
    header_ok && target_ok
}

fn forbidden(code: &str) -> Response {
    let mut res = err(StatusCode::FORBIDDEN, code).into_response();
    res.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    res
}

async fn guard(req: Request, next: Next) -> Response {
    if browser_marked(&req) {
        return forbidden("browser_origin_forbidden");
    }
    if req.method() == Method::OPTIONS {
        return forbidden("preflight_forbidden");
    }
    if !host_is_ours(&req) {
        return forbidden("host_forbidden");
    }
    let mut res = next.run(req).await;
    // Nothing below sets these; dropped anyway so no handler can ever open the
    // API to a page by adding one.
    let h = res.headers_mut();
    let cors: Vec<_> = h
        .keys()
        .filter(|k| k.as_str().starts_with("access-control-"))
        .cloned()
        .collect();
    for k in cors {
        h.remove(k);
    }
    h.insert("cache-control", HeaderValue::from_static("no-store"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    res
}

// ---- handlers ----

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": "shardx-launcher",
        "version": DRIVER_PROTOCOL_VERSION,
    }))
}

async fn list_profiles() -> ApiResult {
    crate::space::ready().await;
    let metas =
        crate::space::profile::list_all().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let running = crate::process::Tracker::shared().running();
    let by_id: std::collections::HashMap<String, crate::process::RunningProfile> = running
        .into_iter()
        .map(|r| (r.profile_id.clone(), r))
        .collect();
    // Tag ids, as `GET /tags` names them.
    let tagged = space::tags::list()
        .map(|s| s.by_profile)
        .unwrap_or_default();
    let out: Vec<Value> = metas
        .into_iter()
        .map(|m| {
            let r = by_id.get(&m.id);
            json!({
                "id": m.id,
                "name": m.name,
                "notes": m.notes,
                "proxy_id": m.proxy_id,
                "last_launched_at": m.last_launched_at,
                "created_at": m.created_at,
                "pinned": m.pinned,
                "folder": m.folder,
                "color": m.color,
                "extensions": m.extensions,
                "protected": m.protected,
                "tags": tagged.get(&m.id).cloned().unwrap_or_default(),
                "running": r.is_some(),
                "pid": r.map(|x| x.pid),
                "cdp": r.and_then(|x| x.cdp.clone()),
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

async fn get_profile(Path(id): Path<String>) -> ApiResult {
    crate::space::ready().await;
    let stored = crate::space::profile::fetch(&id)
        .await
        .map_err(fail(StatusCode::NOT_FOUND))?;
    let mut val = serde_json::to_value(stored).map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("tags".into(), json!(profile_tags(&id)));
    }
    if let Some(cdp) = crate::process::Tracker::shared().cdp(&id) {
        if let Some(obj) = val.as_object_mut() {
            obj.insert("running".into(), json!(true));
            obj.insert(
                "cdp".into(),
                serde_json::to_value(cdp).unwrap_or(Value::Null),
            );
        }
    }
    Ok(Json(val))
}

// ---- create-profile ----

#[derive(Deserialize)]
struct CreateReq {
    name: Option<String>,
    notes: Option<String>,
    proxy_id: Option<String>,
    /// Proxy string: added to store + full-tested, bound by id.
    proxy: Option<String>,
    folder: Option<String>,
    /// Icon and omnibox-pill accent, `#rrggbb`. Omitted = derived from the name.
    color: Option<String>,
    /// Extension-library ids to load at launch (`GET /extensions`).
    extensions: Option<Vec<String>>,
    /// Claimed display refresh rate in Hz (24-480). Absent = the engine's 60.
    refresh_rate: Option<i64>,
    fingerprint: Value,
}

/// Persist verbatim (enrich=false); proxy_id binds, proxy string upserts+tests.
async fn persist_created(
    door: Door,
    t: &Ticket,
    folder_override: Option<String>,
    body: CreateReq,
) -> ApiResult {
    let mut cfg = body
        .fingerprint
        .as_object()
        .cloned()
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "`fingerprint` must be an object"))?;
    cfg.remove("_meta");
    if let Some(n) = body.name.as_ref() {
        cfg.insert("name".into(), json!(n));
    }
    if let Some(n) = body.notes.as_ref() {
        cfg.insert("notes".into(), json!(n));
    }

    let folder = folder_override.or(body.folder).unwrap_or_default();
    // The profile's own door first: a refused one stores no proxy either.
    let made = space::profile::Made {
        proxy_id: body.proxy_id.as_deref(),
        inline_proxy: body.proxy.is_some(),
        extensions: body.extensions.as_ref().is_some_and(|e| !e.is_empty()),
    };
    space::profile::check_create(door, t, &folder, &made).map_err(fail(StatusCode::FORBIDDEN))?;
    let mut meta = json!({ "id": "", "folder": folder });
    if let Some(c) = body.color.as_ref().filter(|c| !c.is_empty()) {
        meta["color"] = json!(c);
    }
    if let Some(ids) = body.extensions.as_ref() {
        validate_extension_ids(ids)?;
        meta["extensions"] = json!(ids);
    }
    let mut inline_proxy = false;
    if let Some(pid) = body.proxy_id.as_ref() {
        meta["proxy_id"] = json!(pid);
    } else if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::space::proxy::parse_single(pstr).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unparseable proxy: {pstr}"),
            )
        })?;
        let stored = crate::space::proxy::upsert_dedup(Door("proxy_save"), t, entry)
            .await
            .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
        // Best-effort full test (UDP + geo); launch re-probes UDP live anyway.
        let _ = crate::space::proxy::full_test(&stored).await;
        meta["proxy_id"] = json!(stored.id);
        inline_proxy = true;
        crate::notify_store_changed("proxies");
    }
    crate::ensure_default_noise(&mut cfg);
    if let Some(hz) = body.refresh_rate {
        apply_refresh_rate(&mut cfg, hz).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    cfg.insert("_meta".into(), meta);

    let stored: space::profile::StoredProfile =
        serde_json::from_value(Value::Object(cfg)).map_err(fail(StatusCode::BAD_REQUEST))?;
    let opts = space::profile::SaveOpts {
        inline_proxy,
        ..Default::default()
    };
    let pm = space::profile::save(door, t, stored, opts)
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    crate::notify_store_changed("profiles");
    Ok(Json(serde_json::to_value(pm).unwrap_or(Value::Null)))
}

async fn create_profile(Extension(t): Extension<Ticket>, Json(body): Json<CreateReq>) -> ApiResult {
    persist_created(Door("POST /profiles"), &t, None, body).await
}

#[derive(Deserialize)]
struct TempReq {
    fingerprint_id: Option<String>,
    platform: Option<String>,
    /// Inline proxy (not stored).
    proxy: Option<String>,
    name: Option<String>,
    folder: Option<String>,
    /// Per vector: `{"canvas": true}` or a full block. Omitted vectors stay off.
    noise: Option<Value>,
    /// Claimed display refresh rate in Hz (24-480). Absent = the engine's 60.
    refresh_rate: Option<i64>,
}

/// Temporary profile (hidden, auto-deleted on close); pair with /start.
async fn create_temporary(Extension(t): Extension<Ticket>, Json(body): Json<TempReq>) -> ApiResult {
    // Kept on this machine and started at once: not a team's yet.
    if t.team().is_some() {
        return Err(refusal(&CmdError::NotOffered(space::TEAM_NOT_YET)));
    }
    let fid = match body.fingerprint_id {
        Some(f) => f,
        None => random_fingerprint_for(body.platform.as_deref())?,
    };
    let mut cfg = crate::build_fingerprint_config(crate::main_window().as_ref(), &fid)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    cfg.remove("_meta");
    if let Some(n) = body.name.as_ref() {
        cfg.insert("name".into(), json!(n));
    }
    crate::ensure_default_noise(&mut cfg);
    if let Some(n) = body.noise.as_ref() {
        apply_noise_overrides(&mut cfg, n).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    if let Some(hz) = body.refresh_rate {
        apply_refresh_rate(&mut cfg, hz).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    let mut meta =
        json!({ "id": "", "folder": body.folder.unwrap_or_default(), "temporary": true });
    if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::space::proxy::parse_single(pstr).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unparseable proxy: {pstr}"),
            )
        })?;
        meta["inline_proxy"] = serde_json::to_value(entry).unwrap_or(Value::Null);
    }
    cfg.insert("_meta".into(), meta);

    t.still().map_err(|e| refusal(&e))?;
    let pm = crate::save_profile_here(crate::main_window().as_ref(), Value::Object(cfg), false)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({
        "id": pm.id,
        "name": pm.name,
        "fingerprint_id": fid,
        "temporary": true,
        "proxy_inline": body.proxy.is_some(),
    })))
}

/// Write `screen.refresh_rate` into a config. Its own field rather than part of
/// `fingerprint` because it is the one piece of the screen block an operator
/// sets on its own: a page reads it by timing frames, not by asking.
fn apply_refresh_rate(cfg: &mut serde_json::Map<String, Value>, hz: i64) -> Result<(), String> {
    if !(24..=480).contains(&hz) {
        return Err(format!("refresh_rate must be between 24 and 480, got {hz}"));
    }
    let screen = cfg
        .entry("screen")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    match screen.as_object_mut() {
        Some(o) => {
            o.insert("refresh_rate".into(), json!(hz));
            Ok(())
        }
        None => Err("`screen` is not an object".into()),
    }
}

/// Merge a caller's noise request into the config's block. Merged, not
/// replaced: `{"webgl": true}` must not zero its intensity.
fn apply_noise_overrides(
    cfg: &mut serde_json::Map<String, Value>,
    req: &Value,
) -> Result<(), String> {
    const VECTORS: &[&str] = &[
        "canvas",
        "webgl",
        "audio",
        "client_rects",
        "sensors",
        "fonts",
    ];
    let req = req.as_object().ok_or("`noise` must be an object")?;
    let noise = cfg
        .get_mut("noise")
        .and_then(|n| n.as_object_mut())
        .ok_or("profile has no noise block")?;

    for (key, val) in req {
        let key = key.as_str();
        if !VECTORS.contains(&key) {
            return Err(format!("unknown noise vector `{key}`"));
        }
        let slot = noise
            .entry(key.to_string())
            .or_insert_with(|| json!({ "enabled": false, "seed": 0 }));
        let Some(slot) = slot.as_object_mut() else {
            continue;
        };
        match val {
            Value::Bool(on) => {
                slot.insert("enabled".into(), json!(on));
                // The defaults the UI writes for the two vectors that carry a
                // strength; turning one on with a zero strength is a no-op.
                if *on
                    && key == "webgl"
                    && slot.get("intensity").and_then(|v| v.as_f64()) == Some(0.0)
                {
                    slot.insert("intensity".into(), json!(0.0005));
                }
                if *on
                    && key == "client_rects"
                    && slot.get("max_offset").and_then(|v| v.as_f64()) == Some(0.0)
                {
                    slot.insert("max_offset".into(), json!(1));
                }
            }
            Value::Object(fields) => {
                for (k, v) in fields {
                    slot.insert(k.clone(), v.clone());
                }
            }
            _ => return Err(format!("`noise.{key}` must be a bool or an object")),
        }
    }
    Ok(())
}

/// Into the trash, like the UI's delete: this machine's, or the team's on its
/// server. Temporary profiles are torn down by the Tracker and never reach it.
async fn delete_profile(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    space::profile::remove(Door("DELETE /profiles/:id"), &t, &id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("profiles");
    Ok(Json(json!({ "deleted": true, "id": id, "trashed": true })))
}

/// Reject unknown ids rather than writing a profile that launches without them.
fn validate_extension_ids(ids: &[String]) -> Result<(), ApiError> {
    let known: std::collections::HashSet<String> = crate::space::extensions::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?
        .into_iter()
        .map(|e| e.id)
        .collect();
    let missing: Vec<&str> = ids
        .iter()
        .filter(|id| !known.contains(*id))
        .map(|s| s.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(err(
            StatusCode::BAD_REQUEST,
            format!("no such extension: {}", missing.join(", ")),
        ))
    }
}

/// A protected profile asked for without its password, or with a wrong one, is a 401; the rest as `fail`.
fn fail_locked(fallback: StatusCode) -> impl Fn(anyhow::Error) -> ApiError {
    move |e| {
        let m = e.to_string();
        if m == space::profile::PASSWORD_REQUIRED || m == "wrong password" {
            return err(StatusCode::UNAUTHORIZED, m);
        }
        fail(fallback)(e)
    }
}

#[derive(Deserialize, Default)]
struct StartReq {
    #[serde(default)]
    headless: bool,
    /// A protected profile's password (#127).
    #[serde(default)]
    password: Option<String>,
}

/// Launch with CDP; body `{ "headless": true, "password": "…" }`, both optional.
async fn start_profile(Path(id): Path<String>, body: Option<Json<StartReq>>) -> ApiResult {
    let Json(body) = body.unwrap_or_default();
    let outcome = crate::launch::launch_profile_unlocked(
        &id,
        true,
        body.headless,
        0,
        "",
        body.password.as_deref(),
    )
    .await
    .map_err(fail_locked(StatusCode::INTERNAL_SERVER_ERROR))?;
    let headless = body.headless;
    Ok(Json(json!({
        "profile_id": id,
        "pid": outcome.pid,
        "headless": headless,
        "cdp": outcome.cdp,
        "cdp_error": outcome.cdp_error,
    })))
}

async fn stop_profile(Path(id): Path<String>) -> ApiResult {
    // Only the space in use: another space's profile is not there for the API.
    if !space::profile::in_active_space(&id) {
        return Err(refusal(&CmdError::Missing(format!("no profile {id} here"))));
    }
    let stopped = crate::process::Tracker::shared()
        .kill(&id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "profile_id": id, "stopped": stopped })))
}

async fn export_cookies(Path(id): Path<String>) -> ApiResult {
    let pass = crate::space::perm::cookies(&id).map_err(fail(StatusCode::FORBIDDEN))?;
    let cookies =
        crate::space::cookies::export(&pass).map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "cookies": cookies })))
}

#[derive(Deserialize)]
struct ImportCookiesReq {
    cookies: Vec<crate::space::cookies::Cookie>,
}

async fn import_cookies(Path(id): Path<String>, Json(body): Json<ImportCookiesReq>) -> ApiResult {
    // Running browser would clobber imports on exit.
    if crate::is_profile_running(&id) {
        return Err(err(
            StatusCode::CONFLICT,
            "stop the profile before importing cookies",
        ));
    }
    let pass = crate::space::perm::cookies(&id).map_err(fail(StatusCode::FORBIDDEN))?;
    let n = crate::space::cookies::import(&pass, &body.cookies)
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "imported": n })))
}

/// The profiles of the space in use that are running.
async fn list_running() -> Json<Value> {
    Json(json!(space::running()))
}

async fn list_fingerprints() -> ApiResult {
    let all = crate::fingerprints::list_all().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let out: Vec<Value> = all
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id,
                "label": e.label,
                "platform": e.platform,
                "chrome": e.chrome,
                "gpu": e.gpu,
                "builtin": e.builtin,
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

async fn list_proxies() -> ApiResult {
    crate::space::ready().await;
    let list = crate::space::proxy::list().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    // Credentials never exposed over API.
    let out: Vec<Value> = list
        .into_iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "kind": p.kind,
                "host": p.host,
                "port": p.port,
                "country": p.country,
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

/// A profile's tag ids.
fn profile_tags(id: &str) -> Vec<String> {
    space::tags::list()
        .ok()
        .and_then(|s| s.by_profile.get(id).cloned())
        .unwrap_or_default()
}

async fn list_trash() -> ApiResult {
    let list = crate::space::trash::list().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)))
}

/// Deletes the archive for good. There is nothing after this.
async fn purge_trash(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    crate::space::trash::purge(&t, &id).map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("trash");
    Ok(Json(json!({ "purged": true, "id": id })))
}

/// Normalize platform string to library tag vocabulary.
fn normalize_platform(p: &str) -> String {
    match p.trim().to_lowercase().as_str() {
        "windows" | "win" => "Windows".into(),
        "linux" => "Linux".into(),
        "mac" | "macos" | "osx" | "darwin" => "macOS".into(),
        "android" | "phone" | "mobile" => "Android".into(),
        other => other.to_string(),
    }
}

/// Random fingerprint id for platform (host OS when None); falls back to all.
fn random_fingerprint_for(platform: Option<&str>) -> Result<String, ApiError> {
    let want = platform
        .map(normalize_platform)
        .unwrap_or_else(crate::host_platform);
    let all = crate::fingerprints::list_all().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    if all.is_empty() {
        return Err(err(StatusCode::NOT_FOUND, "fingerprint library is empty"));
    }
    let matching: Vec<crate::fingerprints::LibraryEntry> = all
        .iter()
        .filter(|e| e.platform.eq_ignore_ascii_case(&want))
        .cloned()
        .collect();
    let pool = if matching.is_empty() { all } else { matching };
    let idx = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % pool.len();
    Ok(pool[idx].id.clone())
}

// ---- server ----

/// Listen on 127.0.0.1:40325 until the app ends. A secret shorter than
/// `MIN_SECRET_LEN` would sign guessable tokens, so the server does not start.
pub async fn serve(secret: String) {
    if secret.len() < MIN_SECRET_LEN {
        eprintln!("[launcher] driver API not started: signing secret too short");
        return;
    }
    set_secret(&secret);
    let app = router();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], API_PORT));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            eprintln!("[launcher] driver API listening on http://{addr}");
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("[launcher] API server error: {e}");
            }
        }
        Err(e) => eprintln!("[launcher] API bind {addr} failed: {e}"),
    }
}

/// Every route, assembled. Only what the agent's session lifecycle uses:
/// profiles (create, temporary, read, delete, start, stop, cookies), what is
/// running, the trash, and the two reads it checks a start against (the
/// fingerprint library and the saved proxies, credentials left out).
fn router() -> Router {
    let protected = Router::new()
        .route("/profiles", get(list_profiles).post(create_profile))
        .route("/profiles/temporary", post(create_temporary))
        .route("/profiles/:id", get(get_profile).delete(delete_profile))
        .route("/profiles/:id/start", post(start_profile))
        .route("/profiles/:id/stop", post(stop_profile))
        .route(
            "/profiles/:id/cookies",
            get(export_cookies).post(import_cookies),
        )
        .route("/running", get(list_running))
        .route("/trash", get(list_trash))
        .route("/trash/:id", delete(purge_trash))
        .route("/fingerprints", get(list_fingerprints))
        .route("/proxies", get(list_proxies))
        .route_layer(middleware::from_fn(in_space))
        .route_layer(middleware::from_fn(auth));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(middleware::from_fn(unless_locked))
        .layer(middleware::from_fn(guard))
}

/// While the data is being moved or reset (the lock in migrate.rs), every
/// route answers 503: nothing starts, and nothing is written into folders on
/// their way out.
async fn unless_locked(req: Request, next: Next) -> Response {
    if crate::migrate::in_progress() {
        return err(StatusCode::SERVICE_UNAVAILABLE, crate::launch::LOCKED).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const SECRET: &str = "0123456789abcdef0123456789abcdef-api-test";

    /// A request as the agent sends it: our Host, no browser headers.
    fn agent(method: &str, path: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, API_HOST)
    }

    fn bearer() -> String {
        set_secret(SECRET);
        format!("Bearer {}", mint(SECRET, 60))
    }

    async fn send(req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
        let res = router().oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn assert_no_cors(headers: &axum::http::HeaderMap, what: &str) {
        let cors: Vec<_> = headers
            .keys()
            .filter(|k| k.as_str().starts_with("access-control-"))
            .collect();
        assert!(cors.is_empty(), "{what} answered with {cors:?}");
    }

    /// Two routes on one path panic when the router is built, so building it
    /// is the whole test.
    #[test]
    fn router_builds() {
        let _ = router();
    }

    /// `/health` is the one route that stays open, for probing before a token.
    #[tokio::test]
    async fn health_stays_open_to_the_agent() {
        let _serial = crate::migrate::serial();
        let (status, headers, body) =
            send(agent("GET", "/health").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "shardx-launcher");
        assert_eq!(body["version"], DRIVER_PROTOCOL_VERSION);
        assert_no_cors(&headers, "/health");
    }

    /// Any browser header, on the open route and on a protected one with a
    /// valid token alike, is refused before the route runs.
    #[tokio::test]
    async fn a_browser_request_is_refused_on_every_route() {
        let _serial = crate::migrate::serial();
        let marks = [
            ("origin", "https://hub.lotusrain.net"),
            ("origin", "null"),
            ("referer", "https://evil.example/page"),
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-site", "same-origin"),
            ("sec-fetch-mode", "no-cors"),
            ("sec-fetch-dest", "empty"),
        ];
        for (method, path) in [
            ("GET", "/health"),
            ("GET", "/profiles"),
            ("POST", "/profiles/x/start"),
            ("GET", "/running"),
        ] {
            for (name, value) in marks {
                let req = agent(method, path)
                    .header(AUTHORIZATION, bearer())
                    .header(name, value)
                    .body(Body::empty())
                    .unwrap();
                let (status, headers, body) = send(req).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path} with {name}");
                assert_eq!(
                    body["error"], "browser_origin_forbidden",
                    "{method} {path} with {name}"
                );
                assert_no_cors(&headers, path);
            }
        }
    }

    /// A CORS or Private Network Access preflight gets nothing that would let
    /// the page go on.
    #[tokio::test]
    async fn preflights_are_refused_without_cors_or_pna_headers() {
        let _serial = crate::migrate::serial();
        for path in ["/health", "/profiles", "/profiles/x/stop"] {
            let pna = agent("OPTIONS", path)
                .header("origin", "https://evil.example")
                .header("access-control-request-method", "POST")
                .header("access-control-request-private-network", "true")
                .body(Body::empty())
                .unwrap();
            let (status, headers, _) = send(pna).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
            assert_no_cors(&headers, path);
            // A bare OPTIONS, no browser header at all, is refused too.
            let (status, headers, body) =
                send(agent("OPTIONS", path).body(Body::empty()).unwrap()).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("preflight_forbidden")),
                "{path}"
            );
            assert_no_cors(&headers, path);
        }
    }

    /// DNS rebinding: a page on a name that resolves to 127.0.0.1 still sends
    /// its own name as Host.
    #[tokio::test]
    async fn a_foreign_host_is_refused() {
        let _serial = crate::migrate::serial();
        for host in [
            "localhost:40325",
            "evil.example",
            "evil.example:40325",
            "127.0.0.1",
            "127.0.0.1:40326",
            "[::1]:40325",
            "127.0.0.1:40325.evil.example",
        ] {
            for path in ["/health", "/profiles"] {
                let req = Request::builder()
                    .uri(path)
                    .header(HOST, host)
                    .header(AUTHORIZATION, bearer())
                    .body(Body::empty())
                    .unwrap();
                let (status, _, body) = send(req).await;
                assert_eq!(
                    (status, body["error"].as_str()),
                    (StatusCode::FORBIDDEN, Some("host_forbidden")),
                    "{host} {path}"
                );
            }
        }
        let missing = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(missing).await.0, StatusCode::FORBIDDEN);
        let twice = agent("GET", "/health")
            .header(HOST, "evil.example")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(twice).await.0, StatusCode::FORBIDDEN);
        let absolute = Request::builder()
            .uri("http://evil.example/health")
            .header(HOST, API_HOST)
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(absolute).await.0, StatusCode::FORBIDDEN);
    }

    /// Every lifecycle route the agent uses is there and needs a token.
    #[tokio::test]
    async fn the_kept_routes_need_a_token() {
        let _serial = crate::migrate::serial();
        for (method, path) in [
            ("GET", "/profiles"),
            ("POST", "/profiles"),
            ("POST", "/profiles/temporary"),
            ("GET", "/profiles/x"),
            ("DELETE", "/profiles/x"),
            ("POST", "/profiles/x/start"),
            ("POST", "/profiles/x/stop"),
            ("GET", "/profiles/x/cookies"),
            ("POST", "/profiles/x/cookies"),
            ("GET", "/running"),
            ("GET", "/trash"),
            ("DELETE", "/trash/x"),
            ("GET", "/fingerprints"),
            ("GET", "/proxies"),
        ] {
            let (status, _, _) = send(agent(method, path).body(Body::empty()).unwrap()).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} answered without a token"
            );
        }
    }

    /// The rest of the old surface is gone, token or not.
    #[tokio::test]
    async fn the_cut_routes_are_gone() {
        let _serial = crate::migrate::serial();
        for (method, path) in [
            ("GET", "/automation/projects"),
            ("POST", "/automation/projects/x/run"),
            ("GET", "/automation/modules"),
            ("GET", "/extensions"),
            ("POST", "/extensions/updates/apply"),
            ("POST", "/proxies"),
            ("DELETE", "/proxies/x"),
            ("POST", "/proxies/x/change-ip"),
            ("GET", "/templates"),
            ("POST", "/templates/x/profiles"),
            ("GET", "/folders"),
            ("POST", "/folders/f/profiles"),
            ("GET", "/tags"),
            ("PUT", "/profiles/x/tags"),
            ("GET", "/bookmarks"),
            ("PATCH", "/profiles/x"),
            ("POST", "/trash/x/restore"),
            ("GET", "/fingerprint/new"),
        ] {
            let req = agent(method, path)
                .header(AUTHORIZATION, bearer())
                .body(Body::empty())
                .unwrap();
            let status = send(req).await.0;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path} still answers: {status}"
            );
        }
    }

    #[test]
    fn only_short_lived_tokens_from_our_secret_pass() {
        let now = 1_800_000_000;
        assert!(verify_at(SECRET, &mint_at(SECRET, now, now + 60), now));
        assert!(
            verify_at(SECRET, &mint_at(SECRET, now, now + 60), now + 89),
            "exp + 30 s skew"
        );
        assert!(
            !verify_at(SECRET, &mint_at(SECRET, now, now + 60), now + 90),
            "past exp + skew"
        );
        assert!(
            verify_at(SECRET, &mint_at(SECRET, now + 30, now + 90), now),
            "iat 30 s ahead"
        );
        assert!(
            !verify_at(SECRET, &mint_at(SECRET, now + 31, now + 91), now),
            "iat beyond skew"
        );
        assert!(
            !verify_at(SECRET, &mint_at(SECRET, now, now + 61), now),
            "longer than 60 s"
        );
        assert!(
            !verify_at(SECRET, &mint_at(SECRET, now, now + 315_360_000), now),
            "the old 10-year token"
        );
        assert!(
            !verify_at(SECRET, &mint_at(SECRET, now, now), now),
            "exp not after iat"
        );
        let other = "fedcba9876543210fedcba9876543210-other";
        assert!(
            !verify_at(other, &mint_at(SECRET, now, now + 60), now),
            "another secret"
        );
        let short = "too-short-secret";
        assert!(
            !verify_at(short, &mint_at(short, now, now + 60), now),
            "secret under 32 bytes"
        );
    }

    /// The token the agent signs (Go `shardx.TokenFromSecret`): header
    /// `{"alg":"HS256","typ":"JWT"}`, claims sub/iat/exp, base64url without padding.
    #[test]
    fn the_agents_token_shape_passes() {
        use base64::Engine;
        use hmac::Mac;
        let enc = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let now = unix_now();
        let signing = format!(
            "{}.{}",
            enc(br#"{"alg":"HS256","typ":"JWT"}"#),
            enc(format!(r#"{{"exp":{},"iat":{now},"sub":"shardx-api"}}"#, now + 60).as_bytes())
        );
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(signing.as_bytes());
        let token = format!("{signing}.{}", enc(&mac.finalize().into_bytes()));
        assert!(verify(SECRET, &token));
    }

    #[tokio::test]
    async fn a_long_lived_token_is_refused_over_http() {
        let _serial = crate::migrate::serial();
        set_secret(SECRET);
        let req = agent("GET", "/running")
            .header(AUTHORIZATION, format!("Bearer {}", mint(SECRET, 600)))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(req).await.0, StatusCode::UNAUTHORIZED);
        let req = agent("GET", "/running")
            .header(AUTHORIZATION, bearer())
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(req).await.0, StatusCode::OK);
    }

    /// I4: every route, open or not, answers 503 while the lock is held.
    #[tokio::test]
    async fn every_route_waits_while_the_data_lock_is_held() {
        let _serial = crate::migrate::serial();
        assert!(crate::migrate::hold());
        let mut answers = Vec::new();
        for (method, path) in [
            ("GET", "/health"),
            ("GET", "/profiles"),
            ("POST", "/profiles/x/start"),
        ] {
            answers.push(
                send(agent(method, path).body(Body::empty()).unwrap())
                    .await
                    .0,
            );
        }
        crate::migrate::release();
        assert!(
            answers
                .iter()
                .all(|s| *s == StatusCode::SERVICE_UNAVAILABLE),
            "{answers:?}"
        );
    }

    async fn authed(method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = agent(method, path).header(AUTHORIZATION, bearer());
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let (status, _, body) = send(req).await;
        (status, body)
    }

    /// The agent's lifecycle over the trimmed API: create, read, delete into
    /// the trash, purge.
    #[tokio::test]
    async fn the_agents_profile_lifecycle_still_works() {
        let _serial = crate::migrate::serial();
        let _root = crate::store::test_root("api-lifecycle").with_config();
        let (status, made) = authed(
            "POST",
            "/profiles",
            Some(json!({ "name": "lotusrain:t1", "fingerprint": {} })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{made}");
        let id = made["id"].as_str().unwrap().to_string();
        let path = format!("/profiles/{id}");
        let (status, got) = authed("GET", &path, None).await;
        assert_eq!(
            (status, got["name"].as_str()),
            (StatusCode::OK, Some("lotusrain:t1")),
            "{got}"
        );
        let (_, all) = authed("GET", "/profiles", None).await;
        assert_eq!(all.as_array().map(Vec::len), Some(1), "{all}");
        assert_eq!(
            authed("GET", "/running", None).await,
            (StatusCode::OK, json!([]))
        );

        let (status, got) = authed("DELETE", &path, None).await;
        assert_eq!(
            (status, got["trashed"].as_bool()),
            (StatusCode::OK, Some(true)),
            "{got}"
        );
        let (_, trash) = authed("GET", "/trash", None).await;
        assert_eq!(trash.as_array().map(Vec::len), Some(1), "{trash}");
        let (status, got) = authed("DELETE", &format!("/trash/{id}"), None).await;
        assert_eq!(
            (status, got["purged"].as_bool()),
            (StatusCode::OK, Some(true)),
            "{got}"
        );
        let (_, trash) = authed("GET", "/trash", None).await;
        assert_eq!(trash, json!([]));
    }
}
