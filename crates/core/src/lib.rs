//! What the Rex float tools do outside the screen: Rex and SSH calls, the
//! runtime config, the project catalog and dev hosts, work names (typed or named on launch), prompts and the images pasted into them, a project's open work items, launching work,
//! agent sessions and their transcripts and slash commands, reviews in tuicr, the task registry, and the inbox's
//! worktree actions.
//! It also holds where switchyard-server listens and how to ask it.

pub mod actions;
pub mod autoname;
pub mod cards;
pub mod changes;
pub mod commands;
pub mod config;
pub mod folders;
pub mod hosts;
pub mod images;
pub mod item_actions;
pub mod item_view;
pub mod items;
pub mod launch;
pub mod links;
pub mod naming;
pub mod orphans;
pub mod prompts;
pub mod review;
pub mod rex;
pub mod server;
pub mod sessions;
pub mod tasks;
pub mod titles;
pub mod transcript;
pub mod usage;
