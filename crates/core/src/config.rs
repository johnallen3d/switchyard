//! Runtime settings, written by the Home Manager module (nix/module.nix) to
//! `$XDG_CONFIG_HOME/switchyard/config.toml` (default `~/.config`): the
//! project catalog, the dev hosts, the `auto` pools, the palette and each
//! profile's default harness. Read once per process; the tools that need the
//! catalog fail with a clear error when the file is missing, and the palette
//! falls back to Rosé Pine.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::rex::{home, local_label};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Rex host labels of the Linux dev machines the pickers offer.
    pub hosts: Vec<String>,
    /// Hosts `auto` chooses from per profile group.
    pub pools: HashMap<String, Vec<String>>,
    /// Rosé Pine role names to `#rrggbb`.
    pub palette: HashMap<String, String>,
    pub projects: Vec<ProjectEntry>,
    /// Read-only quota caches, session roots and optional API-equivalent prices.
    pub usage: crate::usage::Settings,
    /// Where `switchyard-server` and the task registry live.
    pub control_plane: ControlPlane,
    /// switchyard-server's MCP endpoint for agents on the tailnet.
    pub mcp: Mcp,
    /// `[inbox]`: what the tab picker's Inbox leaves out.
    pub inbox: Inbox,
    /// `[harnesses]`: each profile group's default `HARNESS[:MODEL]` for
    /// new work (see `naming::default_harness`).
    pub harnesses: HashMap<String, String>,
}

/// `[control_plane]`: which host runs `switchyard-server` and owns the task
/// registry. When unset, macOS is the control plane (the original setup).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ControlPlane {
    /// Rex host label of the control-plane host (e.g. `"omarchy"`). Empty
    /// keeps the legacy default where the Mac is the control plane.
    pub host: String,
    /// HTTP base URL of the control-plane server for clients on other hosts
    /// (e.g. `"http://omarchy.taila14c2.ts.net:7391"`). Ignored on the
    /// control-plane host itself, which uses its local socket and registry.
    pub url: String,
}

/// `[inbox]`: what the tab picker's Inbox and cmd+k float leave out.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Inbox {
    /// Folders on the config host whose agent tabs stay out of the Inbox:
    /// each one and everything under it. `~` is home; a folder reached
    /// through a symlink matches its target too.
    pub exclude: Vec<String>,
}

/// `[mcp]`: switchyard-server's MCP endpoint (see the server's `mcp`).
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Mcp {
    /// Serve `/mcp`. When unset (`None`), enabled on the control-plane host.
    pub enabled: Option<bool>,
    /// The TCP port, on loopback and the Tailscale address, shared by `/mcp`
    /// and the remote control-plane HTTP API.
    pub port: u16,
}

impl Default for Mcp {
    fn default() -> Self {
        Mcp {
            enabled: None,
            port: 7391,
        }
    }
}

impl Config {
    /// Whether `local` is the control-plane host under this config.
    #[must_use]
    pub fn is_control_plane_on(&self, local: &str) -> bool {
        let host = self.control_plane.host.trim();
        if !host.is_empty() {
            return host == local;
        }
        if !self.control_plane.url.trim().is_empty() {
            return false;
        }
        cfg!(target_os = "macos")
    }

    /// The remote control-plane HTTP base URL (no trailing slash) for a client
    /// on `local`, or `None` when `local` is the control plane or no URL is
    /// configured.
    #[must_use]
    pub fn remote_control_url_on(&self, local: &str) -> Option<&str> {
        if self.is_control_plane_on(local) {
            return None;
        }
        let url = self.control_plane.url.trim().trim_end_matches('/');
        (!url.is_empty()).then_some(url)
    }

    /// The Rex label of the control-plane host: `control_plane.host` when set,
    /// else `local`.
    #[must_use]
    pub fn control_host_on(&self, local: &str) -> String {
        let host = self.control_plane.host.trim();
        if host.is_empty() {
            local.to_string()
        } else {
            host.to_string()
        }
    }

    /// Whether `/mcp` should be served on `local`.
    #[must_use]
    pub fn mcp_enabled_on(&self, local: &str) -> bool {
        self.mcp
            .enabled
            .unwrap_or_else(|| self.is_control_plane_on(local))
    }
}

/// Whether this host is the Switchyard control plane.
#[must_use]
pub fn is_control_plane() -> bool {
    let local = local_label();
    load().map_or(cfg!(target_os = "macos"), |c| c.is_control_plane_on(&local))
}

/// The configured remote control-plane URL for this host, or `None` when this
/// host is the control plane or none is configured.
#[must_use]
pub fn remote_control_url() -> Option<String> {
    let local = local_label();
    load()
        .ok()
        .and_then(|c| c.remote_control_url_on(&local).map(String::from))
}

/// Whether this host can access a control plane (either locally or via a
/// configured remote URL).
#[must_use]
pub fn has_control_plane() -> bool {
    let local = local_label();
    load().map_or(cfg!(target_os = "macos"), |c| {
        c.is_control_plane_on(&local) || c.remote_control_url_on(&local).is_some()
    })
}

/// The explicitly configured `control_plane.host`, if any.
#[must_use]
pub fn configured_control_host() -> Option<String> {
    load().ok().and_then(|c| {
        let host = c.control_plane.host.trim();
        (!host.is_empty()).then(|| host.to_string())
    })
}

/// The Rex label of the control-plane host: `control_plane.host` when set,
/// else this host's own label.
#[must_use]
pub fn control_host() -> String {
    configured_control_host().unwrap_or_else(local_label)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectEntry {
    pub profile: String,
    pub path: String,
    pub clone_url: String,
    pub hosts: Vec<String>,
    /// What the project's refs mean (see `links::Tracker`):
    /// `github:OWNER/REPO` and/or `fizzy:ACCOUNT/BOARD`.
    pub tracker: String,
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map_or_else(|| home().join(".config"), PathBuf::from)
        .join("switchyard/config.toml")
}

/// The config, or why it couldn't be read.
///
/// # Errors
///
/// Fails, as `read` does, when the config file is missing or invalid; the
/// first outcome is kept for the process's lifetime.
pub fn load() -> Result<&'static Config, String> {
    static CONFIG: OnceLock<Result<Config, String>> = OnceLock::new();
    CONFIG.get_or_init(read).as_ref().map_err(Clone::clone)
}

/// The config as the file says now, for a long-running process (the
/// server) that should notice an activation; `load` reads it once.
///
/// # Errors
///
/// Fails when the config file can't be read or isn't valid TOML for
/// `Config`.
pub fn read() -> Result<Config, String> {
    let path = path();
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "can't read {} ({e}); Home Manager writes it, so activate this host's configuration",
            path.display()
        )
    })?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_plane_defaults_to_legacy_behavior_when_unset() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.is_control_plane_on("m4-mbp"), cfg!(target_os = "macos"));
        assert_eq!(cfg.remote_control_url_on("m4-mbp"), None);
        assert_eq!(cfg.control_host_on("m4-mbp"), "m4-mbp");
        assert_eq!(cfg.mcp_enabled_on("m4-mbp"), cfg!(target_os = "macos"));
        assert_eq!(cfg.mcp.port, 7391);
    }

    #[test]
    fn control_plane_host_and_url_distinguish_server_from_clients() {
        let cfg: Config = toml::from_str(
            r#"
            [control_plane]
            host = "omarchy"
            url = "http://omarchy.taila14c2.ts.net:7391/"
            "#,
        )
        .unwrap();
        // On Omarchy: local control plane, no remote URL, MCP enabled by default.
        assert!(cfg.is_control_plane_on("omarchy"));
        assert_eq!(cfg.remote_control_url_on("omarchy"), None);
        assert_eq!(cfg.control_host_on("omarchy"), "omarchy");
        assert!(cfg.mcp_enabled_on("omarchy"));
        // On Mac: client of Omarchy, trailing slash stripped, MCP disabled by default.
        assert!(!cfg.is_control_plane_on("m4-mbp"));
        assert_eq!(
            cfg.remote_control_url_on("m4-mbp"),
            Some("http://omarchy.taila14c2.ts.net:7391")
        );
        assert_eq!(cfg.control_host_on("m4-mbp"), "omarchy");
        assert!(!cfg.mcp_enabled_on("m4-mbp"));
    }

    #[test]
    fn explicit_mcp_enabled_overrides_default() {
        let off: Config = toml::from_str(
            r#"
            [control_plane]
            host = "omarchy"
            [mcp]
            enabled = false
            port = 8000
            "#,
        )
        .unwrap();
        assert!(!off.mcp_enabled_on("omarchy"));
        assert_eq!(off.mcp.port, 8000);

        let on: Config = toml::from_str(
            r#"
            [control_plane]
            host = "omarchy"
            [mcp]
            enabled = true
            "#,
        )
        .unwrap();
        assert!(on.mcp_enabled_on("m4-mbp"));
    }

    #[test]
    fn url_only_control_plane_marks_host_as_client() {
        let cfg: Config = toml::from_str(
            r#"
            [control_plane]
            url = "http://omarchy.taila14c2.ts.net:7391"
            "#,
        )
        .unwrap();
        assert!(!cfg.is_control_plane_on("m4-mbp"));
        assert_eq!(
            cfg.remote_control_url_on("m4-mbp"),
            Some("http://omarchy.taila14c2.ts.net:7391")
        );
    }
}
