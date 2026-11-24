//! Ter Router AI integration.

pub mod client;
pub mod sse;
pub mod types;

pub use client::{ClientOptions, TerRouterClient, ValidationReport};
pub use types::*;
