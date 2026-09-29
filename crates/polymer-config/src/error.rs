use std::{error::Error as StdError, path::PathBuf};

use consolespec::input::BindingKey;
use thiserror::Error;

use crate::ast::SettingPath;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("toml parsing: {0}")]
    TomlError(#[from] toml_spanner::Error),
    #[error("the schema was incorrect")]
    SchemaError { context: &'static str },
    #[error("the definition does not match the schema: {0}")]
    DefinitionError(#[from] toml_spanner::FromTomlError),
    #[error("invalid binding key: {0}")]
    BindingError(#[from] consolespec::input::ParseKeyError),
    #[error("invalid projection: {0}")]
    ProjectionError(Box<str>),
    #[error("a configuration path cannot contain an empty component")]
    InvalidConfigurationPath,
    #[error("unknown serialization target `{0}`")]
    UnknownTarget(Box<str>),
    #[error("target graph contains a cycle at `{0}`")]
    TargetCycle(Box<str>),
    #[error("section `{section}` and target `{target}` disagree about repetition")]
    RepeatMismatch { section: Box<str>, target: Box<str> },
    #[error("repeated target `{0}` has no parent to contain it")]
    RepeatWithoutParent(Box<str>),
    #[error("target `{target}` writes format `{format}`, which has no attributes")]
    AttributeUnsupportedFormat { target: Box<str>, format: Box<str> },
    #[error("target `{0}` is a document root and has no element to hold an attribute")]
    AttributeOnDocumentRoot(Box<str>),
    #[error("the definition has no input configuration")]
    NoInputDefinition,
    #[error("unknown console `{0}`")]
    UnknownConsole(Box<str>),
    #[error("unknown port group `{group}` for console `{console}`")]
    UnknownPortGroup { console: Box<str>, group: Box<str> },
    #[error(
        "port group `{group}` for console `{console}` expects at most {expected} assignments, got {actual}"
    )]
    TooManyPortAssignments {
        console: Box<str>,
        group: Box<str>,
        expected: usize,
        actual: usize,
    },
    #[error("port {index} is not declared in group `{group}`")]
    UnknownPort { group: Box<str>, index: usize },
    #[error("active port {index} in group `{group}` has no virtual gamepad binding")]
    MissingGamepadBinding { group: Box<str>, index: usize },
    #[error("port {index} in group `{group}` is bound more than once")]
    DuplicatePortGamepadBinding { group: Box<str>, index: usize },
    #[error("port {index} in group `{group}` is not active in this configuration")]
    UnexpectedGamepadBinding { group: Box<str>, index: usize },
    #[error("virtual gamepad {index} is bound to more than one emulator port")]
    DuplicateGamepadBinding { index: usize },
    #[error("unknown input layout `{0}`")]
    UnknownLayout(Box<str>),
    #[error("port {index} in group `{group}` does not accept layout `{layout}`")]
    LayoutNotAccepted {
        group: Box<str>,
        index: usize,
        layout: Box<str>,
    },
    #[error("more than one setting materialized at `{0}`")]
    DuplicateSetting(SettingPath),
    #[error("configuration setting `{0}` does not exist")]
    UnknownSetting(Box<str>),
    #[error("section `{0}` does not repeat and cannot receive an instance")]
    NotRepeatable(Box<str>),
    #[error("configuration setting `{path}` expects {expected}, got {actual}")]
    InvalidSettingValue {
        path: Box<str>,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("configuration setting `{path}` value {value} is outside {min}..={max}")]
    OutOfRange {
        path: Box<str>,
        value: String,
        min: String,
        max: String,
    },
    #[error("unknown boolean style `{0}`")]
    UnknownBoolStyle(Box<str>),
    #[error("setting `{path}` selects unknown choice `{choice}`")]
    UnknownChoice { path: Box<str>, choice: Box<str> },
    #[error("choice `{choice}` selected by setting `{path}` does not resolve to a scalar")]
    NonScalarChoice { path: Box<str>, choice: Box<str> },
    #[error("filesystem path cannot be represented in a text configuration: {0:?}")]
    NonUtf8Path(PathBuf),
    #[error("no emitter is registered for output format `{0}`")]
    UnsupportedFormat(Box<str>),
    #[error("more than one target writes output path `{0}`")]
    DuplicateOutputPath(Box<str>),
    #[error("input layout `{layout}` has no SDL projection for `{key}`")]
    MissingProjection { layout: Box<str>, key: BindingKey },
    #[error("input mapping has no value for projected control `{0}`")]
    MissingInputMapping(BindingKey),
    #[error("input layout `{layout}` projects composite control `{key}` to two SDL controls")]
    CompositeProjection { layout: Box<str>, key: BindingKey },
    #[error("unknown input template field in `{0}`")]
    UnknownTemplateField(Box<str>),
    #[error("emitter failed: {0}")]
    Emitter(#[source] Box<dyn StdError + Send + Sync>),
}

impl Error {
    pub fn schema(error: &'static str) -> Self {
        Self::SchemaError { context: error }
    }

    pub fn projection(message: impl Into<Box<str>>) -> Self {
        Self::ProjectionError(message.into())
    }

    pub fn emitter(error: impl StdError + Send + Sync + 'static) -> Self {
        Self::Emitter(Box::new(error))
    }

    pub fn emitter_message(message: impl Into<String>) -> Self {
        Self::emitter(std::io::Error::other(message.into()))
    }

    pub fn diagnostics(&self) -> &[toml_spanner::Error] {
        match self {
            Self::DefinitionError(errors) => &errors.errors,
            _ => &[],
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::emitter(error)
    }
}
