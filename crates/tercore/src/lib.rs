//! TerCode core engine.
//!
//! Every subsystem lives here so it can be unit-tested without a display, and
//! the Tauri shell in `terstudio` stays a thin, replaceable adapter.

pub mod agent;
pub mod ai;
pub mod logging;
pub mod luau;
pub mod mcp;
pub mod project;
pub mod roblox;
pub mod secrets;
pub mod settings;

pub use error::{ErrorPayload, Result, TerError};
pub use logging::{ActivityLog, LogEntry, LogLevel, LogSource};

pub mod error;

/// Version string shown in the UI and sent in the `User-Agent` header.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_NAME: &str = "TerCode";
pub const USER_AGENT: &str = concat!("TerCode/", env!("CARGO_PKG_VERSION"));
