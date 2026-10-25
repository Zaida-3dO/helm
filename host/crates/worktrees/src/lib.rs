//! Worktrees on Windows: where they go, what they are called, how a finished one is removed, and
//! the facts about a clone that Helm's repository list keeps.
//!
//! | Module | Holds |
//! |---|---|
//! | [`rules`] | the short root, name and length rules, and path comparison |
//! | [`links`] | finding and removing links (junctions, symlinks) without following them |
//! | [`remove`] | the removal rule and the removal itself |
//! | [`config`] | the remote, default branch and clone check read from a `.git` folder |
//! | [`git`] | finding the real git and running it |
//! | [`offlist`] | the file listing checkouts whose worktrees are off |
//! | [`argv`] | recognising `git worktree add` and `remove` on a command line |
//!
//! The `helm-gitshim` crate builds the git stand-in on top of [`argv`], [`offlist`], [`links`]
//! and [`git`].

pub mod argv;
pub mod config;
pub mod git;
pub mod links;
pub mod offlist;
pub mod remove;
pub mod rules;
