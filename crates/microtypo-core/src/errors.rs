use serde_json::{Value, json};

/// The error class of the reference an [`Error`] stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// `MicroTypoConfigError`: the configuration or a call argument is invalid.
    Config,
    /// `MicroTypoInputError`: the input is too long, malformed or exhausts a capacity.
    Input,
    /// `MicroTypoBudgetError`: processing ran past `maxProcessingMs`.
    Budget,
    /// `TypeError`: a call argument is malformed.
    Type,
}

/// An error of the engine, with the `name`, `message`, `code` and `details` of the reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// The `details` object; `Null` when the reference sets none.
    pub details: Value,
}

impl Error {
    /// A `MicroTypoConfigError`.
    pub fn config(message: impl Into<String>, details: Value) -> Self {
        Error { kind: ErrorKind::Config, message: message.into(), details }
    }

    /// A `MicroTypoInputError`.
    pub fn input(message: impl Into<String>, details: Value) -> Self {
        Error { kind: ErrorKind::Input, message: message.into(), details }
    }

    /// A `MicroTypoBudgetError`.
    pub fn budget(message: impl Into<String>, details: Value) -> Self {
        Error { kind: ErrorKind::Budget, message: message.into(), details }
    }

    /// A `TypeError`, which has no `details`.
    pub fn type_error(message: impl Into<String>) -> Self {
        Error { kind: ErrorKind::Type, message: message.into(), details: Value::Null }
    }

    /// The `code`: `ERR_MICROTYPO_CONFIG`, `ERR_MICROTYPO_INPUT`, `ERR_MICROTYPO_BUDGET`, or none for a `TypeError`.
    pub fn code(&self) -> Option<&'static str> {
        match self.kind {
            ErrorKind::Config => Some("ERR_MICROTYPO_CONFIG"),
            ErrorKind::Input => Some("ERR_MICROTYPO_INPUT"),
            ErrorKind::Budget => Some("ERR_MICROTYPO_BUDGET"),
            ErrorKind::Type => None,
        }
    }

    /// The `name`: the reference error class.
    pub fn name(&self) -> &'static str {
        match self.kind {
            ErrorKind::Config => "MicroTypoConfigError",
            ErrorKind::Input => "MicroTypoInputError",
            ErrorKind::Budget => "MicroTypoBudgetError",
            ErrorKind::Type => "TypeError",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name(), self.message)
    }
}

impl std::error::Error for Error {}

/// The result of an engine call.
pub type Result<T> = std::result::Result<T, Error>;

pub const MAX_ENTRIES: usize = 100_000;

pub fn capacity_error(message: impl Into<String>, details: Value) -> Error {
    let mut merged = json!({ "reason": "capacity" });
    if let (Some(m), Value::Object(d)) = (merged.as_object_mut(), details) {
        for (k, v) in d {
            m.insert(k, v);
        }
    }
    Error::input(message, merged)
}
