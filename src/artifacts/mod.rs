//! Artifact sync: carries Claude Code state beyond conversation history
//! (settings, skills, agents, commands, plugin manifests, plans, todos,
//! prompt history) between `~/.claude` and the sync repository.
//!
//! Sessions keep their dedicated pipeline in `crate::sync`; this module owns
//! everything category-shaped. Adding a category = one registry row plus one
//! config toggle.

pub mod denylist;
pub mod engine;
pub mod registry;
pub mod union_jsonl;
