use configparser::ini::{Ini, WriteOptions};
use std::io::Write;

use crate::error::Error;

use super::{EmitResult, EmittedValue, Emitter, SectionRef};

/// An in-memory INI emitter backed by [`configparser`].
pub struct IniEmitter {
    document: Ini,
    sections: Vec<Box<str>>,
    write_options: WriteOptions,
}

impl IniEmitter {
    pub fn new() -> Self {
        Self::with_write_options(WriteOptions::default())
    }

    pub fn with_write_options(write_options: WriteOptions) -> Self {
        let mut document = Ini::new_cs();
        // An empty target component is invalid, so this cannot collide with a
        // named target section. configparser omits this section's header.
        document.set_default_section("");
        Self {
            document,
            sections: Vec::new(),
            write_options,
        }
    }

    pub fn document(&self) -> &Ini {
        &self.document
    }

    fn current_section(&self) -> &str {
        self.sections.last().map_or("", AsRef::as_ref)
    }
}

impl Default for IniEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Emitter for IniEmitter {
    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        self.sections.push(section.name.into());
        Ok(())
    }

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
        let rendered = value.value.render()?.into_owned();
        let section = self.current_section().to_owned();
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
        writer.write_all(self.document.pretty_writes(&self.write_options).as_bytes())?;
        Ok(())
    }
}
