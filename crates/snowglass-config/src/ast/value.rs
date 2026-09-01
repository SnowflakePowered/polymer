use std::path::PathBuf;

/// A value stored in a realized configuration or configuration instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The option exists but its definition supplied no default.
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(Box<str>),
    Path(PathBuf),
    /// A stable select-domain key, distinct from its emulator-facing value.
    Choice(Box<str>),
}

impl Value {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Boolean(value) => (*value).into(),
            Self::Integer(value) => (*value).into(),
            Self::Float(value) => (*value).into(),
            Self::String(value) | Self::Choice(value) => value.as_ref().into(),
            Self::Path(value) => value.to_string_lossy().into_owned().into(),
        }
    }
}

/// The stable key of a select-domain choice.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChoiceKey(Box<str>);

impl ChoiceKey {
    pub fn new(key: impl Into<Box<str>>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ChoiceKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for ChoiceKey {
    fn from(key: &str) -> Self {
        Self::new(key)
    }
}

impl From<String> for ChoiceKey {
    fn from(key: String) -> Self {
        Self::new(key)
    }
}
