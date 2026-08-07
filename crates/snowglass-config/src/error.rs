use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("toml parsing: {0}")]
    TomlError(#[from] toml_spanner::Error),
    #[error("the schema was incorrect")]
    SchemaError { context: &'static str },
}

impl Error {
    pub fn schema(error: &'static str) -> Self {
        Self::SchemaError { context: error }
    }
}
