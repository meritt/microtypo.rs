#![forbid(unsafe_code)]

pub mod js;

pub mod api;
pub mod clear_special;
pub mod core;
pub mod engine;
pub mod errors;
pub mod input;
pub mod protect;
pub mod rule_runner;
pub mod rules;
pub mod schemas;
pub mod shared;

pub use engine::{DEFAULT_MAX_INPUT_LENGTH, MicroTypo, Position, Processed, microtypo};
pub use errors::{Error, ErrorKind, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
