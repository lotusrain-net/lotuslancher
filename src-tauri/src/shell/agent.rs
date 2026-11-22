//! The embedded Go agent: started as a child (`--embedded --driver-path`),
//! reached over its instance IPC (`instance.json`: a random loopback port and a
//! per-launch secret, sent as the bare Authorization value).
//!
//! No Windows job object is put around the child: on a shell crash the agent
//! must outlive it long enough to finish its own cleanup and debts.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// The agent's data folder, as `platform.DataDir()` in lotusrain_proxy names it.
pub fn data_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::config_dir();
    Ok(base.context("agent data dir unavailable")?.join("LotusRain Proxy"))
}

/// `LOTUS_AGENT_PATH` (development), else the agent beside this executable.
pub fn agent_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("LOTUS_AGENT_PATH") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("executable has no folder")?;
    let name = if cfg!(windows) { "agent.exe" } else { "lotusrain-agent" };
    Ok(dir.join(name))
}

pub fn spawn() -> Result<()> {
    let mut slot = CHILD.lock().map_err(|_| anyhow!("agent lock poisoned"))?;
    if let Some(c) = slot.as_mut() {
        if c.try_wait()?.is_none() {
            return Ok(());
        }
    }
    let agent = agent_path()?;
    if !agent.is_file() {
        bail!("agent_missing");
    }
    let me = std::env::current_exe()?;
    let mut cmd = Command::new(&agent);
    cmd.arg("--embedded")
        .arg("--driver-path")
        .arg(&me)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(control) = super::CONTROL_OVERRIDE {
        cmd.arg("--control-url").arg(control);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    *slot = Some(cmd.spawn().with_context(|| format!("starting {}", agent.display()))?);
    Ok(())
}

/// The child has exited (or none was started by this shell).
pub fn exited() -> bool {
    match CHILD.lock() {
        Ok(mut g) => match g.as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => true,
        },
        Err(_) => true,
    }
}

/// Last resort after the quit budget: the agent has kept its debts on disk.
pub fn kill() {
    if let Ok(mut g) = CHILD.lock() {
        if let Some(c) = g.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[derive(Deserialize, Clone)]
struct Instance {
    url: String,
    secret: String,
}

fn instance() -> Result<Instance> {
    let raw = std::fs::read(data_dir()?.join("instance.json")).context("agent_not_running")?;
    let info: Instance = serde_json::from_slice(&raw).context("agent_instance_invalid")?;
    check_loopback(&info.url)?;
    if info.secret.is_empty() {
        bail!("agent_instance_invalid");
    }
    Ok(info)
}

/// The agent's address is http on the IPv4 loopback, with nothing else in it.
fn check_loopback(raw: &str) -> Result<()> {
    let u = url::Url::parse(raw).map_err(|_| anyhow!("agent_instance_invalid"))?;
    let ok = u.scheme() == "http"
        && u.host_str() == Some("127.0.0.1")
        && u.port().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && matches!(u.path(), "" | "/")
        && u.query().is_none()
        && u.fragment().is_none();
    if !ok {
        bail!("agent_instance_invalid");
    }
    Ok(())
}

fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().no_proxy().timeout(timeout).redirect(reqwest::redirect::Policy::none()).build()?)
}

/// One IPC call. The agent answers `{"data": …}` or `{"error": code}`; an
/// error comes back as its code alone.
pub async fn call(method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value> {
    let info = instance()?;
    let url = format!("{}{}", info.url.trim_end_matches('/'), path);
    let c = client(timeout)?;
    let req = match method {
        "GET" => c.get(&url),
        _ => c.post(&url),
    }
    .header("Authorization", &info.secret);
    let req = match body {
        Some(b) => req.json(b),
        None if method == "GET" => req,
        None => req.json(&serde_json::json!({})),
    };
    let res = req.send().await.map_err(|_| anyhow!("agent_unreachable"))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let code = v.get("error").and_then(Value::as_str).unwrap_or("agent_request_failed");
        bail!("{}", sanitize_code(code));
    }
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

/// Only short snake_case codes cross into the views.
pub fn sanitize_code(code: &str) -> String {
    if !code.is_empty() && code.len() <= 80 && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
        code.to_string()
    } else {
        "operation_failed".into()
    }
}

pub async fn status() -> Result<Value> {
    call("GET", "/desktop/status", None, Duration::from_secs(3)).await
}

/// Wait until the agent answers its status, up to `budget`.
pub async fn wait_ready(budget: Duration) -> Option<Value> {
    let until = tokio::time::Instant::now() + budget;
    loop {
        if let Ok(v) = status().await {
            return Some(v);
        }
        if tokio::time::Instant::now() >= until {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bare_ipv4_loopback_address_is_trusted() {
        assert!(check_loopback("http://127.0.0.1:43123").is_ok());
        assert!(check_loopback("http://127.0.0.1:43123/").is_ok());
        for bad in [
            "https://127.0.0.1:43123",
            "http://localhost:43123",
            "http://[::1]:43123",
            "http://10.0.0.2:43123",
            "http://127.0.0.1",
            "http://u:p@127.0.0.1:43123",
            "http://127.0.0.1:43123/x",
            "http://127.0.0.1:43123/?a=1",
        ] {
            assert!(check_loopback(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn error_codes_are_kept_short_and_plain() {
        assert_eq!(sanitize_code("session_active"), "session_active");
        assert_eq!(sanitize_code("C:\\Users\\x\\secret"), "operation_failed");
        assert_eq!(sanitize_code("bad code"), "operation_failed");
        assert_eq!(sanitize_code(""), "operation_failed");
    }
}
