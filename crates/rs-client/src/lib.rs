//! Client for RealityScan's Remote Command plugin.
//!
//! The plugin turns a running RealityScan instance into a small REST server:
//! commands go in as ordinary CLI strings (`POST /cmds`), report templates are
//! evaluated on demand (`POST /template`), and progress is reported through
//! `GET /status` and pushed events (see [`events`]).
//!
//! This crate has no GTK dependency so it can be tested headless.

mod client;
pub mod command;
pub mod events;
pub mod json;
pub mod path;

pub use client::{Client, Error, SequenceId, Status};
pub use command::{Command, CommandError, CommandSeq};
pub use events::{Event, EventReceiver, SequenceState};
