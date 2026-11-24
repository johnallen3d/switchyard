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

use crate::rex::home;

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
    /// switchyard-server's MCP endpoint for agents on the tailnet.
    pub mcp: Mcp,
    /// `[inbox]`: what the tab picker's Inbox leaves out.
    pub inbox: Inbox,
    /// `[harnesses]`: each profile group's default `HARNESS[:MODEL]` for
    /// new work (see `naming::default_harness`).
    pub harnesses: HashMap<String, String>,
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
    /// Serve it. On by default on the Mac, the only host that can spawn.
    pub enabled: bool,
    /// The TCP port, on loopback and the Tailscale address.
    pub port: u16,
}

impl Default for Mcp {
    fn default() -> Self {
        Mcp {
            enabled: cfg!(target_os = "macos"),
            port: 7391,
        }
    }
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
