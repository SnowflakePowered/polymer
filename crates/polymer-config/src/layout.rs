use crate::ast::{
    Choice, ChoiceKey, ConfigurationDefinition, DefaultDefinition, DomainDefinition,
    OptionDefinition, OptionRange, SettingPath, Value as SettingValue, ValueKind, ValueMismatch,
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

    fn range(key: &str) -> Self {
        Self::invalid(format!("option '{key}' has an invalid range"))
    }
}

struct LayoutGroup {
    key: String,
    options: Vec<LayoutOption>,
}

struct LayoutOption {
    key: String,
    kind: ValueKind,
}

impl LayoutOption {
    const FIELDS: &'static [&'static str] = &[
        "key",
        "title",
        "description",
        "type",
        "default",
        "min",
        "max",
        "choices",
    ];

    fn parse(option: &Value) -> Result<Self, ConfigurationLayoutError> {
        let option = option
            .as_object()
            .filter(|option| {
                option
                    .keys()
                    .all(|key| Self::FIELDS.contains(&key.as_str()))
            })
            .ok_or_else(|| ConfigurationLayoutError::invalid("invalid synthesized option"))?;
        let key = ConfigurationDefinition::required_text(option, "key")?;
        ConfigurationDefinition::required_text(option, "title")?;
        if option
            .get("description")
            .is_some_and(|value| !value.is_string())
        {
            return Err(ConfigurationLayoutError::invalid(format!(
                "option '{key}' has an invalid description"
            )));
        }
        let kind = ConfigurationDefinition::required_text(option, "type")?;
        let default = option.get("default").ok_or_else(|| {
            ConfigurationLayoutError::invalid(format!("option '{key}' has no default"))
        })?;
        let choices = Self::choices(key, kind, option.get("choices"))?;
        let range = Self::range(key, kind, option);
        let mut option = Self {
            key: key.to_owned(),
            kind: match kind {
                "bool" => ValueKind::Boolean,
                "int" => ValueKind::Integer { range: None },
                "float" => ValueKind::Float { range: None },
                "string" => ValueKind::String,
                "path" => ValueKind::Path,
                "select" => ValueKind::Select {
                    domain: DomainDefinition::Static(choices),
                },
                _ => return Err(Self::incompatible(key, "default")),
            },
        };
        option.check(default, "default")?;
        match &mut option.kind {
            ValueKind::Integer { range: slot } | ValueKind::Float { range: slot } => {
                *slot = range?;
            }
            _ => {
                range?;
            }
        }
        option.check(default, "default")?;
        Ok(option)
    }

    fn choices(
        key: &str,
        kind: &str,
        choices: Option<&Value>,
    ) -> Result<Box<[Choice]>, ConfigurationLayoutError> {
        let invalid =
            || ConfigurationLayoutError::invalid(format!("option '{key}' has invalid choices"));
        let choices = match choices {
            Some(Value::Array(choices)) => choices.as_slice(),
            Some(_) => return Err(invalid()),
            None => &[],
        };
        let mut keys = HashSet::new();
        let parsed = choices
            .iter()
            .map(|choice| {
                let choice = choice.as_object()?;
                let key = ConfigurationDefinition::required_text(choice, "key").ok()?;
                let label = ConfigurationDefinition::required_text(choice, "label").ok()?;
                keys.insert(key).then(|| Choice {
                    key: ChoiceKey::new(key),
                    label: label.into(),
                    value: SettingValue::Null,
                })
            })
            .collect::<Option<Box<[Choice]>>>()
            .ok_or_else(invalid)?;
        if (kind == "select") == parsed.is_empty() {
            return Err(invalid());
        }
        Ok(parsed)
    }

    fn range(
        key: &str,
        kind: &str,
        option: &Map<String, Value>,
    ) -> Result<Option<OptionRange>, ConfigurationLayoutError> {
        let (min, max) = (option.get("min"), option.get("max"));
        if min.is_none() && max.is_none() {
            return Ok(None);
        }
        let error = || ConfigurationLayoutError::range(key);
        let range = match kind {
            "int" => {
                let bound = |bound: Option<&Value>, open| bound.map_or(Some(open), Value::as_i64);
                let (min, max) = (bound(min, i64::MIN), bound(max, i64::MAX));
                let (min, max) = (min.ok_or_else(error)?, max.ok_or_else(error)?);
                (min <= max).then_some(OptionRange::Integer { min, max })
            }
            "float" => {
                let bound = |bound: Option<&Value>, open| bound.map_or(Some(open), Value::as_f64);
                let (min, max) = (bound(min, f64::NEG_INFINITY), bound(max, f64::INFINITY));
                let (min, max) = (min.ok_or_else(error)?, max.ok_or_else(error)?);
                (min <= max).then_some(OptionRange::Float { min, max })
            }
            _ => None,
        };
        range.map(Some).ok_or_else(error)
    }

    fn check(&self, value: &Value, role: &str) -> Result<(), ConfigurationLayoutError> {
        self.kind
            .parse_json(value)
            .map(drop)
            .map_err(|mismatch| match mismatch {
                ValueMismatch::Range { .. } => ConfigurationLayoutError::range(&self.key),
                ValueMismatch::Kind(_) | ValueMismatch::Choice(_) => {
                    Self::incompatible(&self.key, role)
                }
            })
    }

    fn incompatible(key: &str, role: &str) -> ConfigurationLayoutError {
        ConfigurationLayoutError::invalid(format!(
            "option '{key}' has a {role} incompatible with its type"
        ))
    }
}

impl ConfigurationDefinition {
    pub fn expand_layout(&self, mut layout: Value) -> Result<Value, ConfigurationLayoutError> {
        let root = layout
            .as_object_mut()
            .ok_or_else(|| ConfigurationLayoutError::invalid("layout must be an object"))?;
        Self::required_text(root, "schema")?;
        root.remove("key");
        let groups = root
            .get_mut("groups")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| ConfigurationLayoutError::invalid("groups must be an array"))?;
        let mut declared = Vec::with_capacity(groups.len());
        for group in groups {
            let group = self.expand_group(group, &declared)?;
            declared.push(group);
        }
        Self::validate_values(root, &declared)?;
        Ok(layout)
    }

    fn expand_group(
        &self,
        group: &mut Value,
        declared: &[LayoutGroup],
    ) -> Result<LayoutGroup, ConfigurationLayoutError> {
        let group = group
            .as_object_mut()
            .ok_or_else(|| ConfigurationLayoutError::invalid("group must be an object"))?;
        let key = Self::required_text(group, "key")?.to_owned();
        Self::required_text(group, "title")?;
        if declared.iter().any(|group| group.key == key) {
            return Err(ConfigurationLayoutError::invalid(format!(
                "duplicate group key '{key}'"
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
        let mut parsed = Vec::<LayoutOption>::with_capacity(options.len());
        for option in options {
            let layout_option = if option.get("source").is_some() {
                let (layout_option, expanded) = self.expand_source(option)?;
                *option = expanded;
                layout_option
            } else {
                LayoutOption::parse(option)?
            };
            if parsed.iter().any(|option| option.key == layout_option.key) {
                return Err(ConfigurationLayoutError::invalid(format!(
                    "duplicate option key '{}' in group '{key}'",
                    layout_option.key
                )));
            }
            parsed.push(layout_option);
        }
        Ok(LayoutGroup {
            key,
            options: parsed,
        })
    }

    fn validate_values(
        root: &Map<String, Value>,
        declared: &[LayoutGroup],
    ) -> Result<(), ConfigurationLayoutError> {
        let values = root
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| ConfigurationLayoutError::invalid("values must be an object"))?;
        for group in declared {
            let selected = values.get(&group.key).and_then(Value::as_object);
            for option in &group.options {
                let value = selected
                    .and_then(|selected| selected.get(&option.key))
                    .ok_or_else(|| ConfigurationLayoutError::UnsettledValue {
                        group: group.key.clone(),
                        option: option.key.clone(),
                    })?;
                option.check(value, "settled value")?;
            }
        }
        for (group_key, selected) in values {
            let selected = selected.as_object().ok_or_else(|| {
                ConfigurationLayoutError::invalid(format!(
                    "configuration values for group '{group_key}' must be an object"
                ))
            })?;
            let Some(group) = declared.iter().find(|group| &group.key == group_key) else {
                let option = selected.keys().next().map_or("<group>", String::as_str);
                return Err(ConfigurationLayoutError::UnknownValue {
                    group: group_key.clone(),
                    option: option.to_owned(),
                });
            };
            if let Some(option) = selected.keys().find(|option| {
                !group
                    .options
                    .iter()
                    .any(|declared| &declared.key == *option)
            }) {
                return Err(ConfigurationLayoutError::UnknownValue {
                    group: group_key.clone(),
                    option: option.clone(),
                });
            }
        }
        Ok(())
    }

    fn expand_source(
        &self,
        source: &Value,
    ) -> Result<(LayoutOption, Value), ConfigurationLayoutError> {
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
                    (SettingPath::input(&option.key).as_str() == path).then_some((option, false))
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
        Ok((
            LayoutOption {
                key: key.to_owned(),
                kind: option.0.value.kind.clone(),
            },
            Self::option_json(key, option.0),
        ))
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
}
