use toml_spanner::{Context, Failed, FromToml, Item};

pub use crate::ast::TargetPath;
use crate::ast::{ConfigurationPath, family};

impl<'de> FromToml<'de> for ConfigurationPath<&'de str, family::Target> {
    fn from_toml(ctx: &mut Context<'de>, item: &Item<'de>) -> Result<Self, Failed> {
        let path = item.require_string(ctx)?;

        if !Self::is_valid(path) {
            return Err(
                ctx.report_custom_error("a target path cannot contain an empty component", item)
            );
        }

        Ok(Self::from_key(path))
    }
}
