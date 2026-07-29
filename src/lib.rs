#![forbid(unsafe_code)]

pub mod broker;
pub mod cli;
pub mod control;
pub mod daemon;
pub mod display;
pub mod peer_identity;
pub mod protocol;
pub mod runtime_path;
pub mod webhook;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
