use configparser::ini::{Ini, WriteOptions};
use serde::Deserialize;
use std::io::Write;

use crate::error::Error;

use super::{EmitResult, EmittedValue, Emitter, ScalarValue, SectionRef};

/// Which INI values are wrapped in double quotes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum IniQuote {
    #[default]
    None,
    Strings,
    All,
}

/// How the INI renderer writes values.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct IniOptions {
    pub quote: IniQuote,
    /// Backslash-escapes `\` and `"` inside quoted values. Without it, a quoted
    /// value that contains a quote or line break is rejected.
    pub escape: bool,
    /// Writes `key = value` instead of `key=value`.
    pub spaces: bool,
}

/// An in-memory INI emitter backed by [`configparser`]. Nested targets become
/// sections named by their dot-joined path below the document root.
pub struct IniEmitter {
    document: Ini,
    sections: Vec<Box<str>>,
    options: IniOptions,
}

impl IniEmitter {
    pub fn new(options: IniOptions) -> Self {
        let mut document = Ini::new_cs();
        // An empty target component is invalid, so this cannot collide with a
        // named target section. configparser omits this section's header.
        document.set_default_section("");
        Self {
            document,
            sections: Vec::new(),
            options,
        }
    }

    fn render(&self, value: &EmittedValue<'_>) -> EmitResult<String> {
        let rendered = value.value.render()?;
        let quoted = match self.options.quote {
            IniQuote::None => false,
            IniQuote::Strings => {
                matches!(value.value, ScalarValue::String(_) | ScalarValue::Path(_))
            }
            IniQuote::All => true,
        };
        if !quoted {
            return Ok(rendered.into_owned());
        }
        if self.options.escape {
            return Ok(format!(
                "\"{}\"",
                rendered.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
        if rendered.contains(['"', '\r', '\n']) {
            return Err(Error::emitter_message(format!(
                "unescaped quoted INI value for `{}` cannot contain quotes or line breaks",
                value.key
            )));
        }
        Ok(format!("\"{rendered}\""))
    }
}

impl Default for IniEmitter {
    fn default() -> Self {
        Self::new(IniOptions::default())
    }
}

impl Emitter for IniEmitter {
    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        section.reject_attributes("ini")?;
        if section.repeated {
            return Err(Error::emitter_message(format!(
                "INI cannot repeat section `{}`",
                section.path
            )));
        }
        self.sections.push(section.name.into());
        Ok(())
    }

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
        let rendered = self.render(&value)?;
        let section = self.sections.join(".");
        self.document.set(&section, value.key, Some(rendered));
        Ok(())
    }

    fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        let entered = self.sections.pop().ok_or_else(|| {
            Error::emitter_message("exited an INI section that was never entered")
        })?;
        if entered.as_ref() != section.name {
            return Err(Error::emitter_message(format!(
                "exited INI section `{}` while `{entered}` was active",
                section.name
            )));
        }
        Ok(())
    }

    fn finish(&mut self, writer: &mut dyn Write) -> EmitResult<()> {
        if let Some(section) = self.sections.last() {
            return Err(Error::emitter_message(format!(
                "INI section `{section}` was not exited"
            )));
        }
        let options = WriteOptions::new_with_params(self.options.spaces, 4, 0);
        writer.write_all(self.document.pretty_writes(&options).as_bytes())?;
        Ok(())
    }
}
