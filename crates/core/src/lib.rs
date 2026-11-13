//! What the Rex float tools do outside the screen: Rex and SSH calls, the
//! runtime config, the project catalog and dev hosts, work names (typed or named on launch) and prompts, launching work,
//! agent sessions and their transcripts and slash commands, the task registry, and the inbox's
//! worktree actions.
//! It also holds where switchyard-server listens and how to ask it.

pub mod actions;
pub mod autoname;
pub mod commands;
pub mod config;
pub mod folders;
pub mod hosts;
pub mod launch;
pub mod links;
pub mod naming;
pub mod prompts;
pub mod rex;
pub mod server;
pub mod sessions;
pub mod tasks;
pub mod transcript;
pub mod usage;
