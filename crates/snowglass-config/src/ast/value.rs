/// A value stored in a realized configuration or configuration instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The option exists but its definition supplied no default.
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(Box<str>),
    /// A stable select-domain key, distinct from its emulator-facing value.
    Choice(Box<str>),
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
