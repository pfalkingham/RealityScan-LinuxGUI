//! Running RealityScan headless under Wine.
//!
//! [`Install`] describes where RealityScan and its Wine runtime live: Heroic's Epic
//! install run through umu/Proton, or Epic's Linux package, a CrossOver bottle on this
//! machine or inside a distrobox container ([`Runtime`]). [`Launcher`] starts an
//! instance with the Remote Command plugin's REST server enabled and hands
//! back an [`Instance`] that owns the process, an [`rs_client::Client`] and,
//! optionally, an [`rs_client::EventReceiver`].

pub mod autosave;
mod install;
mod launch;
pub mod proc_tree;

pub use install::{Install, InstallError, RUNTIME_ENV, Runtime};
pub use launch::{Instance, LaunchError, LaunchOptions, Launcher};
