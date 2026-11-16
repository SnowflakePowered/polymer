use std::io::Write;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::error::Error;

use super::{EmitResult, EmittedValue, Emitter, ScalarValue, SectionRef};

/// An in-memory TOML emitter that writes target sections as nested tables,
/// repeated sections as arrays of tables, and scalars as native TOML values.
#[derive(Default)]
pub struct TomlEmitter {
    document: DocumentMut,
    sections: Vec<(Box<str>, bool)>,
}

impl TomlEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    fn table(&mut self) -> EmitResult<&mut Table> {
        let mut table = self.document.as_table_mut();
        for (name, repeated) in &self.sections {
            let child = if *repeated {
                table
                    .get_mut(name)
                    .and_then(Item::as_array_of_tables_mut)
                    .and_then(|tables| tables.iter_mut().last())
            } else {
                let mut implicit = Table::new();
                implicit.set_implicit(true);
                table
                    .entry(name)
                    .or_insert(Item::Table(implicit))
                    .as_table_mut()
            };
            table = child.ok_or_else(|| {
                Error::emitter_message(format!("TOML key `{name}` is not a table"))
            })?;
        }
        Ok(table)
    }
}

impl Emitter for TomlEmitter {
    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        section.reject_attributes("toml")?;
        if section.repeated {
            self.table()?
                .entry(section.name)
                .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
                .as_array_of_tables_mut()
                .ok_or_else(|| {
                    Error::emitter_message(format!(
                        "TOML key `{}` is not an array of tables",
                        section.name
                    ))
                })?
                .push(Table::new());
        }
        self.sections.push((section.name.into(), section.repeated));
        self.table()?;
        Ok(())
    }

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
        let rendered = match value.value {
            ScalarValue::Boolean { value, style: None } => Value::from(value),
            ScalarValue::Integer { value, style: None } => Value::from(value),
            ScalarValue::Float { value, style: None } => Value::from(value),
            ScalarValue::List(items) => Value::Array(items.iter().map(AsRef::as_ref).collect()),
            scalar => Value::from(scalar.render()?.as_ref()),
        };
        self.table()?.insert(value.key, Item::Value(rendered));
        Ok(())
    }

    fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        let (entered, _) = self.sections.pop().ok_or_else(|| {
            Error::emitter_message("exited a TOML section that was never entered")
        })?;
        if entered.as_ref() != section.name {
            return Err(Error::emitter_message(format!(
                "exited TOML section `{}` while `{entered}` was active",
                section.name
            )));
        }
        Ok(())
    }

    fn finish(&mut self, writer: &mut dyn Write) -> EmitResult<()> {
        if let Some((section, _)) = self.sections.last() {
            return Err(Error::emitter_message(format!(
                "TOML section `{section}` was not exited"
            )));
        }
        writer.write_all(self.document.to_string().as_bytes())?;
        Ok(())
    }
}
