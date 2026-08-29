use snowglass_config::{
    ast::{ConfigurationInstance, OptionRange, Value, ValueKind},
    ser::{BmlEmitter, Emitter, GamepadBinding, IniEmitter, Serializer, XmlEmitter},
};
use std::{collections::BTreeMap, path::PathBuf};
use thiserror::Error;

pub type SerializedConfiguration = BTreeMap<String, Vec<u8>>;

pub(crate) type ConfigurationId = u32;

/// Invocation-scoped configurations exposed to plugin JavaScript. Proxies
/// carry their ID into every op, so no ambient active invocation is required.
#[derive(Default)]
pub(crate) struct ConfigurationInstances {
    next_id: ConfigurationId,
    instances: BTreeMap<ConfigurationId, ConfigurationInstance>,
}

impl ConfigurationInstances {
    pub(crate) fn insert(&mut self, instance: ConfigurationInstance) -> ConfigurationId {
        let id = loop {
            self.next_id = self.next_id.wrapping_add(1);
            if self.next_id == 0 {
                continue;
            }
            if !self.instances.contains_key(&self.next_id) {
                break self.next_id;
            }
        };
        self.instances.insert(id, instance);
        id
    }

    pub(crate) fn get(
        &self,
        id: ConfigurationId,
        path: &str,
    ) -> Result<Option<serde_json::Value>, ConfigurationAccessError> {
        let instance = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        Ok(instance.get(path).map(value_to_json))
    }

    pub(crate) fn set(
        &mut self,
        id: ConfigurationId,
        path: &str,
        value: serde_json::Value,
    ) -> Result<(), ConfigurationAccessError> {
        let instance = self
            .instances
            .get_mut(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        set_value(instance, path, value)?;
        Ok(())
    }

    pub(crate) fn entries(
        &self,
        id: ConfigurationId,
    ) -> Result<Vec<(String, serde_json::Value)>, ConfigurationAccessError> {
        let instance = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        Ok(instance
            .iter()
            .map(|(_, setting)| {
                (
                    setting.path.as_str().to_owned(),
                    value_to_json(&setting.value),
                )
            })
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn take(
        &mut self,
        id: ConfigurationId,
    ) -> Result<ConfigurationInstance, ConfigurationAccessError> {
        self.instances
            .remove(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))
    }

    pub(crate) fn discard(&mut self, id: ConfigurationId) {
        self.instances.remove(&id);
    }

    pub(crate) fn rematerialize(
        &mut self,
        id: ConfigurationId,
        mut replacement: ConfigurationInstance,
    ) -> Result<(), ConfigurationAccessError> {
        let current = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        let values = current
            .iter()
            .map(|(_, setting)| {
                (
                    setting.path.as_str().to_owned(),
                    value_to_json(&setting.value),
                )
            })
            .collect::<Vec<_>>();
        for (path, value) in values {
            if replacement.setting(&path).is_some() {
                set_value(&mut replacement, &path, value)?;
            }
        }
        self.instances.insert(id, replacement);
        Ok(())
    }

    pub(crate) fn render(
        &mut self,
        id: ConfigurationId,
        bindings: Vec<GamepadBinding>,
    ) -> Result<SerializedConfiguration, ConfigurationAccessError> {
        let instance = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        serialize(instance, bindings)
    }
}

pub(crate) fn serialize(
    instance: &ConfigurationInstance,
    bindings: Vec<GamepadBinding>,
) -> Result<SerializedConfiguration, ConfigurationAccessError> {
    let serializer = Serializer::new(instance, bindings)?;
    let files = serializer.serialize(emitter_for)?;
    Ok(files
        .into_iter()
        .map(|(path, bytes)| (path.into(), bytes))
        .collect())
}

pub(crate) fn set_value(
    instance: &mut ConfigurationInstance,
    path: &str,
    value: serde_json::Value,
) -> Result<(), ConfigurationAccessError> {
    let option = instance
        .setting(path)
        .ok_or_else(|| ConfigurationAccessError::UnknownSetting(path.to_owned()))?
        .option;
    let value = value_from_json(instance, path, option, value)?;
    let setting = instance
        .get_mut(path)
        .ok_or_else(|| ConfigurationAccessError::UnknownSetting(path.to_owned()))?;
    *setting = value;
    Ok(())
}

fn emitter_for(format: &str) -> Option<Box<dyn Emitter>> {
    match format {
        "bml" => Some(Box::new(BmlEmitter::new())),
        "ini" => Some(Box::new(IniEmitter::new())),
        "xml" => Some(Box::new(XmlEmitter::new())),
        _ => None,
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => (*value).into(),
        Value::Integer(value) => (*value).into(),
        Value::Float(value) => serde_json::Value::from(*value),
        Value::String(value) | Value::Choice(value) => value.as_ref().into(),
        Value::Path(value) => value.to_string_lossy().into_owned().into(),
    }
}

fn value_from_json(
    instance: &ConfigurationInstance,
    path: &str,
    option: snowglass_config::Handle<snowglass_config::ast::OptionDefinition>,
    value: serde_json::Value,
) -> Result<Value, ConfigurationAccessError> {
    if value.is_null() {
        return Ok(Value::Null);
    }

    let option_ref = instance.definition().option(option);
    let invalid = |expected| ConfigurationAccessError::InvalidSettingValue {
        path: path.to_owned(),
        expected,
        actual: json_kind(&value),
    };

    match &option_ref.definition.value.kind {
        ValueKind::Boolean => value
            .as_bool()
            .map(Value::Boolean)
            .ok_or_else(|| invalid("a boolean")),
        ValueKind::Integer { range } => {
            let value = value.as_i64().ok_or_else(|| invalid("an integer"))?;
            if let Some(OptionRange::Integer { min, max }) = range
                && !(*min..=*max).contains(&value)
            {
                return Err(ConfigurationAccessError::OutOfRange {
                    path: path.to_owned(),
                    value: value.to_string(),
                    min: min.to_string(),
                    max: max.to_string(),
                });
            }
            Ok(Value::Integer(value))
        }
        ValueKind::Float { range } => {
            let value = value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| invalid("a finite number"))?;
            if let Some(OptionRange::Float { min, max }) = range
                && !(*min..=*max).contains(&value)
            {
                return Err(ConfigurationAccessError::OutOfRange {
                    path: path.to_owned(),
                    value: value.to_string(),
                    min: min.to_string(),
                    max: max.to_string(),
                });
            }
            Ok(Value::Float(value))
        }
        ValueKind::String => value
            .as_str()
            .map(|value| Value::String(value.into()))
            .ok_or_else(|| invalid("a string")),
        ValueKind::Path => value
            .as_str()
            .map(|value| Value::Path(PathBuf::from(value)))
            .ok_or_else(|| invalid("a path string")),
        ValueKind::Select { .. } => {
            let choice = value.as_str().ok_or_else(|| invalid("a choice key"))?;
            let known = instance
                .definition()
                .choices(option)
                .is_some_and(|choices| {
                    choices
                        .iter()
                        .any(|candidate| candidate.key.as_str() == choice)
                });
            if !known {
                return Err(ConfigurationAccessError::UnknownChoice {
                    path: path.to_owned(),
                    choice: choice.to_owned(),
                });
            }
            Ok(Value::Choice(choice.into()))
        }
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => "an integer",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

#[derive(Debug, Error)]
pub enum ConfigurationAccessError {
    #[error("plugin configuration invocation {0} does not exist")]
    UnknownConfiguration(ConfigurationId),
    #[error("configuration setting '{0}' does not exist")]
    UnknownSetting(String),
    #[error("configuration setting '{path}' expects {expected}, got {actual}")]
    InvalidSettingValue {
        path: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("configuration setting '{path}' value {value} is outside {min}..={max}")]
    OutOfRange {
        path: String,
        value: String,
        min: String,
        max: String,
    },
    #[error("configuration setting '{path}' has no choice '{choice}'")]
    UnknownChoice { path: String, choice: String },
    #[error(transparent)]
    Configuration(#[from] snowglass_config::error::Error),
}
