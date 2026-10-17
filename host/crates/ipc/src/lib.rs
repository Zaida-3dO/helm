//! Frontend to host IPC.
//!
//! The renderer reaches the host through exactly one command, `api(op, args)`, and hears back on
//! one tagged event channel. This crate owns everything about that command that is not Tauri:
//!
//! - [`OpRegistry`]: the extension seam. A crate adds ops by exposing `register_ops(&mut
//!   OpRegistry)`; the app calls it from one registration line (`host/README.md`).
//! - [`Dispatcher`]: checks the calling window's label and URL, then runs the op in its class:
//!   async, blocking, blocking under the global `maintenance` lock, or blocking under the
//!   two-permit `inspections` semaphore.
//! - [`AppEvent`] and [`EventSink`]: the one event channel, a union tagged by `kind`.

pub mod authority;
pub mod dispatch;
pub mod events;
pub mod registry;
pub mod services;

pub use authority::{is_trusted_origin, Caller, MAIN_WINDOW};
pub use dispatch::Dispatcher;
pub use events::{AppEvent, EventSink, NullSink, RecordingSink, EVENT_CHANNEL};
pub use registry::{ApiError, OpClass, OpCtx, OpRegistry};
pub use services::Services;
