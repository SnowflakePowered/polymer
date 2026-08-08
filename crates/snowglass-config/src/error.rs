use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("toml parsing: {0}")]
    TomlError(#[from] toml_spanner::Error),
    #[error("the schema was incorrect")]
    SchemaError { context: &'static str },
    #[error("the definition does not match the schema: {0}")]
    DefinitionError(#[from] toml_spanner::FromTomlError),
}

impl Error {
    pub fn schema(error: &'static str) -> Self {
        Self::SchemaError { context: error }
    }

    pub fn diagnostics(&self) -> &[toml_spanner::Error] {
        match self {
            Self::DefinitionError(errors) => &errors.errors,
            _ => &[],
        }
    }
}
