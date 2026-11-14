//! Helm worker supervision.
//!
//! | Module | Holds |
//! |---|---|
//! | [`job`] | One kill-on-close Job Object per worker tree (a process group on Linux), optionally holding it at below-normal priority |
//! | [`contrib`] | Launch contributors: `PATH` prefixes, variables and system-prompt fragments from other crates |
//! | [`env`] | The worker environment: registry variables plus identity variables |
//! | [`node`] | Finding Node (`HELM_NODE_PATH`, then `PATH`), refusing shims and old versions |
//! | [`stage`] | Staging `worker/src` once per content hash |
//! | [`launch`] | `launch.json`, `claude-runtime.json`, and the debug-only `HELM_TEST_CLAUDE` seam |
//! | [`priority`] | Below-normal priority for workers and helpers: the Windows creation flag, the Linux nice |
//! | [`process`] | The worker process: JSON records on stdout (96 MiB limit), 16 KB stderr tail |
//! | [`status`] | which Node and Claude Code a worker would use, for Settings → Runtime |
//!
//! This crate knows nothing about chats; `helm-chats` drives it.

pub mod contrib;
pub mod env;
pub mod job;
pub mod launch;
pub mod node;
pub mod priority;
pub mod process;
pub mod stage;
pub mod status;

pub use process::{spawn, SpawnSpec, WorkerOutput, WorkerProcess, MAX_RECORD_BYTES};
