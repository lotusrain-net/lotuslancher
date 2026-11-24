//! The shell's own small persisted state, beside the hub view's data folder:
//! `<local data>/LotusProxy/shell-state.json` and `.../LotusProxy/hub-webview/`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShellState {
    /// Latched before a desktop logout starts and cleared only once the hub
    /// view's storage is wiped. Still set at startup: wipe before loading.
    #[serde(default)]
    pub pending_webview_clear: bool,
    /// The agent was seen authorized while this hub storage was in use. A
    /// later start that finds it signed out (and not mid-grant) means a logout
    /// finished its revocation without the wipe that follows it.
    #[serde(default)]
    pub bound: bool,
    /// The one-time "still running in the tray" notice was shown.
    #[serde(default)]
    pub tray_hint_shown: bool,
}

pub fn root() -> Result<PathBuf> {
    Ok(dirs::data_local_dir()
        .context("OS local data dir unavailable")?
        .join("LotusProxy"))
}

pub fn hub_data_dir() -> Result<PathBuf> {
    Ok(root()?.join("hub-webview"))
}

fn path() -> Result<PathBuf> {
    Ok(root()?.join("shell-state.json"))
}

pub fn load() -> ShellState {
    path()
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Written whole through a temp file, so a crash leaves the old or the new
/// state and never half of one.
pub fn save(s: &ShellState) -> Result<()> {
    let p = path()?;
    std::fs::create_dir_all(p.parent().context("state path has no parent")?)?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

pub fn update(f: impl FnOnce(&mut ShellState)) -> Result<ShellState> {
    let mut s = load();
    f(&mut s);
    save(&s)?;
    Ok(s)
}

/// What the agent reported, as far as the startup wipe cares.
pub struct AgentAuth<'a> {
    pub authorization: &'a str,
    pub authorization_detail: &'a str,
    pub authorization_resolved: bool,
    pub pending_webview_clear: bool,
    pub pending_grant: bool,
    pub pending_logout: bool,
}

/// Whether the hub view's storage must be wiped before any remote page loads.
pub fn must_wipe(shell: &ShellState, agent: Option<&AgentAuth>) -> bool {
    if shell.pending_webview_clear {
        return true;
    }
    let Some(a) = agent else { return false };
    let confirmed_revocation = a.authorization == "reauth_required"
        && matches!(
            a.authorization_detail,
            "device_revoked"
                | "device_seat_revoked"
                | "device_seat_suspended"
                | "seat_revoked"
                | "seat_suspended"
        );
    a.pending_webview_clear
        || (shell.bound
            && a.authorization_resolved
            && confirmed_revocation
            && !a.pending_grant
            && !a.pending_logout)
}

/// Delete the hub view's whole data folder: cookies, localStorage, IndexedDB,
/// cache. Only called before the hub view exists.
pub fn wipe_hub_data() -> Result<()> {
    let dir = hub_data_dir()?;
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", dir.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(authorization: &str) -> AgentAuth<'_> {
        AgentAuth {
            authorization,
            authorization_detail: "",
            authorization_resolved: true,
            pending_webview_clear: false,
            pending_grant: false,
            pending_logout: false,
        }
    }

    #[test]
    fn a_latched_clear_wipes_whatever_the_agent_says() {
        let s = ShellState {
            pending_webview_clear: true,
            ..Default::default()
        };
        assert!(must_wipe(&s, None));
        assert!(must_wipe(&s, Some(&agent("authorized"))));
    }

    #[test]
    fn the_agents_own_latch_wipes() {
        let a = AgentAuth {
            pending_webview_clear: true,
            ..agent("signed_out")
        };
        assert!(must_wipe(&ShellState::default(), Some(&a)));
    }

    #[test]
    fn confirmed_device_revocation_behind_a_bound_view_wipes() {
        let bound = ShellState {
            bound: true,
            ..Default::default()
        };
        let revoked = AgentAuth {
            authorization_detail: "device_seat_revoked",
            ..agent("reauth_required")
        };
        assert!(must_wipe(&bound, Some(&revoked)));
        // Not while a grant is being claimed or a logout is still running.
        assert!(!must_wipe(
            &bound,
            Some(&AgentAuth {
                pending_grant: true,
                ..revoked
            })
        ));
        assert!(!must_wipe(
            &bound,
            Some(&AgentAuth {
                pending_logout: true,
                ..revoked
            })
        ));
        assert!(!must_wipe(&bound, Some(&agent("authorized"))));
    }

    #[test]
    fn missing_or_invalid_client_credentials_do_not_clear_iam_cookies() {
        let bound = ShellState {
            bound: true,
            ..Default::default()
        };
        assert!(!must_wipe(&bound, Some(&agent("signed_out"))));
        let invalid = AgentAuth {
            authorization_detail: "invalid_grant",
            ..agent("reauth_required")
        };
        assert!(!must_wipe(&bound, Some(&invalid)));
    }

    #[test]
    fn startup_signed_out_is_not_authoritative_until_agent_restore_finishes() {
        let bound = ShellState {
            bound: true,
            ..Default::default()
        };
        let restoring = AgentAuth {
            authorization_resolved: false,
            ..agent("signed_out")
        };
        assert!(!must_wipe(&bound, Some(&restoring)));
        assert!(!must_wipe(&bound, Some(&agent("signed_out"))));
    }

    #[test]
    fn a_first_run_or_half_signed_in_view_is_kept() {
        // Never bound: the IAM login of a user whose device grant failed stays.
        assert!(!must_wipe(
            &ShellState::default(),
            Some(&agent("signed_out"))
        ));
        assert!(!must_wipe(&ShellState::default(), None));
    }
}
