#![forbid(unsafe_code)]

pub mod broker;
pub mod display;
pub mod protocol;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
