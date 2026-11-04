use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Config,
    Input,
    Budget,
    Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub details: Value,
}

impl Error {
    pub fn config(message: impl Into<String>, details: Value) -> Self {
        Error { kind: ErrorKind::Config, message: message.into(), details }
    }

    pub fn input(message: impl Into<String>, details: Value) -> Self {
        Error { kind: ErrorKind::Input, message: message.into(), details }
    }

    pub fn budget(message: impl Into<String>, details: Value) -> Self {
        Error { kind: ErrorKind::Budget, message: message.into(), details }
    }

    pub fn type_error(message: impl Into<String>) -> Self {
        Error { kind: ErrorKind::Type, message: message.into(), details: Value::Null }
    }

    pub fn code(&self) -> Option<&'static str> {
        match self.kind {
            ErrorKind::Config => Some("ERR_MICROTYPO_CONFIG"),
            ErrorKind::Input => Some("ERR_MICROTYPO_INPUT"),
            ErrorKind::Budget => Some("ERR_MICROTYPO_BUDGET"),
            ErrorKind::Type => None,
        }
    }

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
