use crate::ast::{
    ConfigurationDefinition, DefaultDefinition, DomainDefinition, OptionDefinition, OptionRange,
    ValueKind,
};
use serde_json::{Map, Value};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigurationLayoutError {
    #[error("invalid configuration layout: {0}")]
    Invalid(String),
    #[error("configuration option '{key}' references unknown setting '{setting}'")]
    UnknownSource { key: String, setting: String },
    #[error("configuration option '{group}.{option}' has no settled value")]
    UnsettledValue { group: String, option: String },
    #[error("configuration value '{group}.{option}' has no visible option")]
    UnknownValue { group: String, option: String },
}

impl ConfigurationLayoutError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl ConfigurationDefinition {
    pub fn expand_layout(&self, mut layout: Value) -> Result<Value, ConfigurationLayoutError> {
        let root = layout
            .as_object_mut()
            .ok_or_else(|| ConfigurationLayoutError::invalid("layout must be an object"))?;
        Self::required_text(root, "schema")?;
        Self::required_text(root, "key")?;
        let groups = root
            .get_mut("groups")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| ConfigurationLayoutError::invalid("groups must be an array"))?;
        let mut group_keys = HashSet::new();

        for group in groups {
            let group = group
                .as_object_mut()
                .ok_or_else(|| ConfigurationLayoutError::invalid("group must be an object"))?;
            let group_key = Self::required_text(group, "key")?.to_owned();
            Self::required_text(group, "title")?;
            if !group_keys.insert(group_key.clone()) {
                return Err(ConfigurationLayoutError::invalid(format!(
                    "duplicate group key '{group_key}'"
                )));
            }
            if group
                .get("description")
                .is_some_and(|value| !value.is_string())
            {
                return Err(ConfigurationLayoutError::invalid(
                    "group description must be a string",
                ));
            }

            let options = group
                .get_mut("options")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| ConfigurationLayoutError::invalid("options must be an array"))?;
            let mut option_keys = HashSet::new();
            for option in options {
                if option.get("source").is_some() {
                    *option = self.expand_source(option)?;
                }
                self.validate_option(option)?;
                let key = option["key"].as_str().unwrap();
                if !option_keys.insert(key.to_owned()) {
                    return Err(ConfigurationLayoutError::invalid(format!(
                        "duplicate option key '{key}' in group '{group_key}'"
                    )));
                }
            }
        }
        self.validate_values(root)?;
        Ok(layout)
    }

    fn validate_values(&self, root: &Map<String, Value>) -> Result<(), ConfigurationLayoutError> {
        let values = root
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| ConfigurationLayoutError::invalid("values must be an object"))?;
        let groups = root["groups"]
            .as_array()
            .expect("groups were validated before values");
        let declared = groups
            .iter()
            .map(|group| {
                let key = group["key"].as_str().unwrap();
                let options = group["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|option| (option["key"].as_str().unwrap(), option))
                    .collect::<std::collections::HashMap<_, _>>();
                (key, options)
            })
            .collect::<std::collections::HashMap<_, _>>();

        for (group_key, options) in &declared {
            let selected = values.get(*group_key).and_then(Value::as_object);
            for (option_key, option) in options {
                let value = selected
                    .and_then(|selected| selected.get(*option_key))
                    .ok_or_else(|| ConfigurationLayoutError::UnsettledValue {
                        group: (*group_key).to_owned(),
                        option: (*option_key).to_owned(),
                    })?;
                self.validate_selected_value(option, value)?;
            }
        }
        for (group_key, selected) in values {
            let selected = selected.as_object().ok_or_else(|| {
                ConfigurationLayoutError::invalid(format!(
                    "configuration values for group '{group_key}' must be an object"
                ))
            })?;
            let Some(options) = declared.get(group_key.as_str()) else {
                let option = selected.keys().next().map_or("<group>", String::as_str);
                return Err(ConfigurationLayoutError::UnknownValue {
                    group: group_key.clone(),
                    option: option.to_owned(),
                });
            };
            if let Some(option) = selected
                .keys()
                .find(|option| !options.contains_key(option.as_str()))
            {
                return Err(ConfigurationLayoutError::UnknownValue {
                    group: group_key.clone(),
                    option: option.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_selected_value(
        &self,
        option: &Value,
        value: &Value,
    ) -> Result<(), ConfigurationLayoutError> {
        let option = option.as_object().unwrap();
        let key = option["key"].as_str().unwrap();
        let kind = option["type"].as_str().unwrap();
        let choices = option
            .get("choices")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let numeric = match kind {
            "bool" if value.is_null() || value.is_boolean() => None,
            "int" if value.is_null() || value.as_i64().is_some() => Some(true),
            "float" if value.is_null() || value.as_f64().is_some() => Some(false),
            "string" | "path" if value.is_null() || value.is_string() => None,
            "select"
                if value.is_null()
                    || value.as_str().is_some_and(|selected| {
                        choices.iter().any(|choice| choice["key"] == selected)
                    }) =>
            {
                None
            }
            _ => {
                return Err(ConfigurationLayoutError::invalid(format!(
                    "option '{key}' has a settled value incompatible with its type"
                )));
            }
        };
        self.validate_range(key, option, value, numeric)
    }

    fn expand_source(&self, source: &Value) -> Result<Value, ConfigurationLayoutError> {
        let source = source
            .as_object()
            .filter(|source| source.len() == 2)
            .ok_or_else(|| ConfigurationLayoutError::invalid("invalid source reference"))?;
        let key = Self::required_text(source, "key")?;
        let path = Self::required_text(source, "source")?;
        if path.starts_with("ports.") {
            return Err(ConfigurationLayoutError::UnknownSource {
                key: key.to_owned(),
                setting: path.to_owned(),
            });
        }
        let option = self
            .sections
            .iter()
            .find_map(|(_, section)| {
                section.options.iter().find_map(|handle| {
                    let option = self.option(*handle);
                    let direct = format!("{}.{}", section.path.as_str(), option.key);
                    let emitted = section.target.as_ref().and_then(|target| {
                        target
                            .as_str()
                            .split_once('.')
                            .map(|(_, target)| format!("{target}.{}", option.key))
                    });
                    let dynamic = self.sections.iter().any(|(_, candidate)| {
                        candidate.repeat
                            && (candidate.path == section.path
                                || candidate.path.is_parent_of(&section.path))
                    });
                    (direct == path || emitted.as_deref() == Some(path))
                        .then_some((option, dynamic))
                })
            })
            .or_else(|| {
                self.input.as_ref()?.options.iter().find_map(|handle| {
                    let option = self.option(*handle);
                    (format!("input.{}", option.key) == path).then_some((option, false))
                })
            })
            .ok_or_else(|| ConfigurationLayoutError::UnknownSource {
                key: key.to_owned(),
                setting: path.to_owned(),
            })?;
        if option.1 {
            return Err(ConfigurationLayoutError::UnknownSource {
                key: key.to_owned(),
                setting: path.to_owned(),
            });
        }
        Ok(Self::option_json(key, option.0))
    }

    fn option_json(key: &str, option: &OptionDefinition) -> Value {
        let (kind, range, choices) = match &option.value.kind {
            ValueKind::Boolean => ("bool", None, None),
            ValueKind::Integer { range } => ("int", *range, None),
            ValueKind::Float { range } => ("float", *range, None),
            ValueKind::String => ("string", None, None),
            ValueKind::Path => ("path", None, None),
            ValueKind::Select { domain } => (
                "select",
                None,
                match domain {
                    DomainDefinition::Static(choices) => Some(
                        choices
                            .iter()
                            .map(|choice| {
                                serde_json::json!({
                                    "key": choice.key.as_str(),
                                    "label": choice.label,
                                })
                            })
                            .collect::<Vec<_>>(),
                    ),
                    DomainDefinition::None => None,
                },
            ),
        };
        let mut output = Map::from_iter([
            ("key".to_owned(), key.into()),
            ("title".to_owned(), option.title.as_ref().into()),
            ("type".to_owned(), kind.into()),
            (
                "default".to_owned(),
                match &option.value.default {
                    DefaultDefinition::None => Value::Null,
                    DefaultDefinition::Literal(value) => value.to_json(),
                },
            ),
        ]);
        if let Some(range) = range {
            let (min, max) = match range {
                OptionRange::Integer { min, max } => (min.into(), max.into()),
                OptionRange::Float { min, max } => (min.into(), max.into()),
            };
            output.extend([("min".to_owned(), min), ("max".to_owned(), max)]);
        }
        if let Some(choices) = choices {
            output.insert("choices".to_owned(), choices.into());
        }
        output.into()
    }

    fn validate_option(&self, option: &Value) -> Result<(), ConfigurationLayoutError> {
        const FIELDS: &[&str] = &[
            "key",
            "title",
            "description",
            "type",
            "default",
            "min",
            "max",
            "choices",
        ];
        let option = option
            .as_object()
            .filter(|option| option.keys().all(|key| FIELDS.contains(&key.as_str())))
            .ok_or_else(|| ConfigurationLayoutError::invalid("invalid synthesized option"))?;
        let key = Self::required_text(option, "key")?;
        Self::required_text(option, "title")?;
        if option
            .get("description")
            .is_some_and(|value| !value.is_string())
        {
            return Err(ConfigurationLayoutError::invalid(format!(
                "option '{key}' has an invalid description"
            )));
        }
        let kind = Self::required_text(option, "type")?;
        let default = option.get("default").ok_or_else(|| {
            ConfigurationLayoutError::invalid(format!("option '{key}' has no default"))
        })?;
        let choices = match option.get("choices") {
            Some(Value::Array(choices)) => choices.as_slice(),
            Some(_) => {
                return Err(ConfigurationLayoutError::invalid(format!(
                    "option '{key}' has invalid choices"
                )));
            }
            None => &[],
        };
        self.validate_choices(key, kind, default, choices)?;

        let numeric = match kind {
            "bool" if default.is_null() || default.is_boolean() => None,
            "int" if default.is_null() || default.as_i64().is_some() => Some(true),
            "float" if default.is_null() || default.as_f64().is_some() => Some(false),
            "string" | "path" if default.is_null() || default.is_string() => None,
            "select"
                if default.is_null()
                    || default.as_str().is_some_and(|selected| {
                        choices.iter().any(|choice| choice["key"] == selected)
                    }) =>
            {
                None
            }
            _ => {
                return Err(ConfigurationLayoutError::invalid(format!(
                    "option '{key}' has a default incompatible with its type"
                )));
            }
        };
        self.validate_range(key, option, default, numeric)
    }

    fn validate_choices(
        &self,
        option: &str,
        kind: &str,
        default: &Value,
        choices: &[Value],
    ) -> Result<(), ConfigurationLayoutError> {
        let mut keys = HashSet::new();
        let valid = choices.iter().all(|choice| {
            choice.as_object().is_some_and(|choice| {
                Self::required_text(choice, "key").is_ok_and(|key| keys.insert(key))
                    && Self::required_text(choice, "label").is_ok()
            })
        });
        if !valid || (kind == "select") != !choices.is_empty() {
            return Err(ConfigurationLayoutError::invalid(format!(
                "option '{option}' has invalid choices"
            )));
        }
        if kind == "select"
            && !default.is_null()
            && !default
                .as_str()
                .is_some_and(|selected| keys.contains(selected))
        {
            return Err(ConfigurationLayoutError::invalid(format!(
                "option '{option}' has a default incompatible with its type"
            )));
        }
        Ok(())
    }

    fn validate_range(
        &self,
        key: &str,
        option: &Map<String, Value>,
        default: &Value,
        integer: Option<bool>,
    ) -> Result<(), ConfigurationLayoutError> {
        if integer.is_none() && (option.contains_key("min") || option.contains_key("max")) {
            return Err(Self::range_error(key));
        }
        let number = |value: &Value| match integer {
            Some(true) => value.as_i64().map(|value| value as f64),
            Some(false) => value.as_f64(),
            None => None,
        };
        let bound = |name| match option.get(name) {
            Some(value) => number(value)
                .ok_or_else(|| Self::range_error(key))
                .map(Some),
            None => Ok(None),
        };
        let min = bound("min")?;
        let max = bound("max")?;
        let outside = number(default).is_some_and(|value| {
            min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max)
        });
        if matches!((min, max), (Some(min), Some(max)) if min > max) || outside {
            return Err(Self::range_error(key));
        }
        Ok(())
    }

    fn required_text<'a>(
        object: &'a Map<String, Value>,
        field: &str,
    ) -> Result<&'a str, ConfigurationLayoutError> {
        object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ConfigurationLayoutError::invalid(format!("{field} must be a non-empty string"))
            })
    }

    fn range_error(key: &str) -> ConfigurationLayoutError {
        ConfigurationLayoutError::invalid(format!("option '{key}' has an invalid range"))
    }
}
