//! Russian typography that never mangles code, URLs or markup: a byte-exact port of the [microtypo](https://github.com/meritt/microtypo) JavaScript engine.
//!
//! Build a [`MicroTypo`] from a [`Config`] and a [`Host`], then call [`MicroTypo::process`]. Output, [`Processed::changes`] and errors match the reference byte for byte.
//!
//! ```
//! use microtypo_core::{Config, MicroTypo};
//! # let host = microtypo_conformance::HOST;
//!
//! let mut engine = MicroTypo::with_config(Config::default(), host)?;
//! assert_eq!(engine.process("\"Амбер\" - город")?, "«Амбер»\u{a0}— город");
//! # Ok::<(), microtypo_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod js;

mod api;
mod clear_special;
mod config;
mod core;
mod diagnostics;
mod engine;
mod errors;
mod host;
mod inline_lift;
mod input;
mod protect;
mod rule_runner;
mod rules;
mod schemas;
mod shared;

pub use api::Output;
pub use config::{Config, DataPattern, DataRule, DataRuleGroup, Input, Render};
pub use diagnostics::{Diagnostics, ProcessOptions};
pub use engine::{DEFAULT_MAX_INPUT_LENGTH, MicroTypo, Position, Processed, microtypo};
pub use errors::{Error, ErrorKind, Result};
pub use host::Host;
pub use js::JsString;
pub use js::regex::{Caps, Re, RegexSyntaxError};
pub use rule_runner::{
    Ctx, CycleHook, HandlerFn, PreParseFn, ReplaceFn, Replacement, Rule, RuleBody, RuleGroupDef, RuleGroupInfo,
    RuleInfo,
};
pub use shared::diff::Edit;
pub use shared::rule_trace::{TraceEntry, TraceKind};

/// Version of this crate, which the bindings report as theirs.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[doc(hidden)]
pub mod internals {
    pub use crate::js::escape;
    pub use crate::js::raw_json::{Node as JsonNode, RawJson};
    pub use crate::js::regex::{builtins, generated, interpreted};
    pub use crate::js::regex::{out_of_budget, with_budget};
    pub use crate::rules::registry::default_groups;
    pub use crate::shared::diff::replacement_edits;
    pub use crate::shared::glyphs::{GLYPH_TO_ENTITY, unicode_to_entities, unicode_to_xml_entities};
}
