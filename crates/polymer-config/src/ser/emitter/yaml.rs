use std::io::Write;

use indexmap::IndexMap;
use rust_yaml::{Value, Yaml, YamlConfig};

use crate::error::Error;

use super::{EmitResult, EmittedValue, Emitter, ScalarValue, SectionRef};

/// An in-memory YAML emitter that writes target sections as nested mappings,
/// repeated sections as sequences of mappings, and scalars as native values.
#[derive(Default)]
pub struct YamlEmitter {
    document: IndexMap<Value, Value>,
    sections: Vec<(Box<str>, bool)>,
}

impl YamlEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    fn mapping(&mut self) -> EmitResult<&mut IndexMap<Value, Value>> {
        let mut mapping = &mut self.document;
        for (name, repeated) in &self.sections {
            let mut entry = mapping
                .entry(Value::String(name.to_string()))
                .or_insert_with(|| Value::Mapping(IndexMap::new()));
            if *repeated && let Value::Sequence(items) = entry {
                entry = items.last_mut().ok_or_else(|| {
                    Error::emitter_message(format!("YAML sequence `{name}` is empty"))
                })?;
            }
            let Value::Mapping(child) = entry else {
                return Err(Error::emitter_message(format!(
                    "YAML key `{name}` is not a mapping"
                )));
            };
            mapping = child;
        }
        Ok(mapping)
    }
}

impl Emitter for YamlEmitter {
    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        section.reject_attributes("yaml")?;
        if section.repeated {
            let entry = self
                .mapping()?
                .entry(Value::String(section.name.to_owned()))
                .or_insert_with(|| Value::Sequence(Vec::new()));
            let Value::Sequence(items) = entry else {
                return Err(Error::emitter_message(format!(
                    "YAML key `{}` is not a sequence",
                    section.name
                )));
            };
            items.push(Value::Mapping(IndexMap::new()));
        }
        self.sections.push((section.name.into(), section.repeated));
        self.mapping()?;
        Ok(())
    }

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
        let rendered = match value.value {
            ScalarValue::Boolean { value, style: None } => Value::Bool(value),
            ScalarValue::Integer { value, style: None } => Value::Int(value),
            ScalarValue::Float { value, style: None } => Value::Float(value),
            ScalarValue::List(items) => Value::Sequence(
                items
                    .iter()
                    .map(|item| Value::String(item.to_string()))
                    .collect(),
            ),
            scalar => Value::String(scalar.render()?.into_owned()),
        };
        self.mapping()?
            .insert(Value::String(value.key.to_owned()), rendered);
        Ok(())
    }

    fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        let (entered, _) = self.sections.pop().ok_or_else(|| {
            Error::emitter_message("exited a YAML section that was never entered")
        })?;
        if entered.as_ref() != section.name {
            return Err(Error::emitter_message(format!(
                "exited YAML section `{}` while `{entered}` was active",
                section.name
            )));
        }
        Ok(())
    }

    fn finish(&mut self, writer: &mut dyn Write) -> EmitResult<()> {
        if let Some((section, _)) = self.sections.last() {
            return Err(Error::emitter_message(format!(
                "YAML section `{section}` was not exited"
            )));
        }
        let document = Value::Mapping(std::mem::take(&mut self.document));
        Yaml::with_config(YamlConfig {
            emit_anchors: false,
            width: None,
            ..YamlConfig::default()
        })
        .dump(&document, writer)
        .map_err(Error::emitter)
    }
}
