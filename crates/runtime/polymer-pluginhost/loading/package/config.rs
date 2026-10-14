use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use thiserror::Error;

use super::PackageError;

/// A `[plugin.config]` option shown to the user.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginOption {
    pub display: String,
    pub default: toml::Value,
    #[serde(default)]
    pub choices: Vec<toml::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum PluginConfigEntry {
    Option(PluginOption),
    Value(toml::Value),
}

/// The `[plugin.config]` table: options with a `display` name the user can change, and
/// fixed values set by the plugin author.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct PluginConfig(BTreeMap<String, PluginConfigEntry>);

#[derive(Debug, Error)]
pub enum PluginConfigError {
    #[error("plugin has no user-facing option '{0}'")]
    UnknownOption(String),
    #[error("{value} is not a valid value for option '{key}'")]
    InvalidValue { key: String, value: Value },
}

impl PluginOption {
    fn json(value: &toml::Value) -> Value {
        serde_json::to_value(value).unwrap_or(Value::Null)
    }

    pub fn default_value(&self) -> Value {
        Self::json(&self.default)
    }

    pub fn choice_values(&self) -> Vec<Value> {
        self.choices.iter().map(Self::json).collect()
    }

    fn accepts(&self, value: &Value) -> bool {
        if !self.choices.is_empty() {
            return self.choice_values().contains(value);
        }
        matches!(
            (self.default_value(), value),
            (Value::Bool(_), Value::Bool(_))
                | (Value::Number(_), Value::Number(_))
                | (Value::String(_), Value::String(_))
        )
    }
}

impl PluginConfig {
    pub(super) fn validate(&self) -> Result<(), PackageError> {
        for (key, entry) in &self.0 {
            let valid = match entry {
                PluginConfigEntry::Option(option) => {
                    !option.display.trim().is_empty()
                        && (option.choices.is_empty() || option.choices.contains(&option.default))
                }
                PluginConfigEntry::Value(toml::Value::Table(table)) => !table.contains_key("display"),
                PluginConfigEntry::Value(_) => true,
            };
            if !valid {
                return Err(PackageError::InvalidConfigOption(key.clone()));
            }
        }
        Ok(())
    }

    /// The options shown to the user, in key order.
    pub fn options(&self) -> impl Iterator<Item = (&str, &PluginOption)> {
        self.0.iter().filter_map(|(key, entry)| match entry {
            PluginConfigEntry::Option(option) => Some((key.as_str(), option)),
            PluginConfigEntry::Value(_) => None,
        })
    }

    /// Every configured value, with each option's saved value when it is still valid.
    pub fn effective(&self, saved: &Map<String, Value>) -> Map<String, Value> {
        self.0
            .iter()
            .map(|(key, entry)| {
                let value = match entry {
                    PluginConfigEntry::Option(option) => saved
                        .get(key)
                        .filter(|value| option.accepts(value))
                        .cloned()
                        .unwrap_or_else(|| option.default_value()),
                    PluginConfigEntry::Value(value) => PluginOption::json(value),
                };
                (key.clone(), value)
            })
            .collect()
    }

    /// Checks a value the user chose for an option.
    pub fn validate_option(&self, key: &str, value: &Value) -> Result<(), PluginConfigError> {
        let (_, option) = self
            .options()
            .find(|(name, _)| *name == key)
            .ok_or_else(|| PluginConfigError::UnknownOption(key.to_owned()))?;
        if option.accepts(value) {
            Ok(())
        } else {
            Err(PluginConfigError::InvalidValue {
                key: key.to_owned(),
                value: value.clone(),
            })
        }
    }
}
