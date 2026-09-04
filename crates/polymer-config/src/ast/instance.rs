use std::{collections::BTreeMap, collections::HashMap, ops, slice, sync::Arc};

use indexmap::IndexMap;

use crate::{
    arena::{Arena, Handle},
    error::{Error, Result},
};

use super::{
    ConfigurationDefinition, DefaultDefinition, DomainDefinition, OptionDefinition, OptionRange,
    SettingPath, Value, ValueKind, family,
};

pub type PortAssignments = BTreeMap<Box<str>, Box<[Box<str>]>>;

/// A launch-specific, flat collection of configuration values.
#[derive(Clone)]
pub struct ConfigurationInstance {
    definition: Arc<ConfigurationDefinition>,
    console: Box<str>,
    port_assignments: PortAssignments,
    settings: Arena<Setting>,
    setting_by_path: HashMap<SettingPath, Handle<Setting>>,
    repeated_instances: IndexMap<Box<str>, u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub path: SettingPath,
    pub option: Handle<OptionDefinition>,
    pub value: Value,
}

impl ConfigurationDefinition {
    pub fn instantiate(
        self: &Arc<Self>,
        console: impl Into<Box<str>>,
        port_assignments: PortAssignments,
    ) -> Result<ConfigurationInstance> {
        let console = console.into();
        let mut builder = InstanceBuilder::new(self.clone(), console, port_assignments);
        builder.instantiate_sections()?;
        builder.instantiate_input_options()?;
        builder.instantiate_ports()?;
        Ok(builder.finish())
    }
}

struct InstanceBuilder {
    definition: Arc<ConfigurationDefinition>,
    console: Box<str>,
    port_assignments: PortAssignments,
    settings: Arena<Setting>,
    setting_by_path: HashMap<SettingPath, Handle<Setting>>,
}

impl InstanceBuilder {
    fn new(
        definition: Arc<ConfigurationDefinition>,
        console: Box<str>,
        port_assignments: PortAssignments,
    ) -> Self {
        Self {
            definition,
            console,
            port_assignments,
            settings: Arena::new(),
            setting_by_path: HashMap::new(),
        }
    }

    fn instantiate_sections(&mut self) -> Result<()> {
        let mut placements = Vec::new();
        for section in self.definition.root_sections() {
            self.collect_section_placements(section, &mut placements);
        }

        for (path, option) in placements {
            ConfigurationInstance::append_setting(
                &self.definition,
                &mut self.settings,
                &mut self.setting_by_path,
                path,
                option,
            )?;
        }
        Ok(())
    }

    fn collect_section_placements(
        &self,
        handle: Handle<super::SectionDefinition>,
        placements: &mut Vec<(SettingPath, Handle<OptionDefinition>)>,
    ) {
        let section = &self.definition.sections[handle];
        if section.repeat {
            return;
        }
        placements.extend(section.options.iter().copied().map(|option| {
            (
                section
                    .path
                    .join::<family::Setting>(&self.definition.options[option].key),
                option,
            )
        }));
        for child in section.children.iter().copied() {
            self.collect_section_placements(child, placements);
        }
    }

    fn instantiate_ports(&mut self) -> Result<()> {
        let definition = &self.definition;
        let Some(input) = definition.input.as_ref() else {
            return if self.port_assignments.is_empty() {
                Ok(())
            } else {
                Err(Error::NoInputDefinition)
            };
        };
        let platform = input
            .platforms
            .get(self.console.as_ref())
            .ok_or_else(|| Error::UnknownConsole(self.console.clone()))?;

        let mut placements = Vec::new();
        for (group_name, assignments) in &self.port_assignments {
            let group =
                platform
                    .get(group_name.as_ref())
                    .ok_or_else(|| Error::UnknownPortGroup {
                        console: self.console.clone(),
                        group: group_name.clone(),
                    })?;
            let port_count = group.iter().map(|port| port.index + 1).max().unwrap_or(0);
            if assignments.len() > port_count {
                return Err(Error::TooManyPortAssignments {
                    console: self.console.clone(),
                    group: group_name.clone(),
                    expected: port_count,
                    actual: assignments.len(),
                });
            }

            for (index, layout_name) in assignments.iter().enumerate() {
                let port = group
                    .iter()
                    .find(|port| port.index == index)
                    .ok_or_else(|| Error::UnknownPort {
                        group: group_name.clone(),
                        index,
                    })?;
                if !port.layouts.contains(layout_name) {
                    return Err(Error::LayoutNotAccepted {
                        group: group_name.clone(),
                        index,
                        layout: layout_name.clone(),
                    });
                }
                let layout = input
                    .layouts
                    .get(layout_name.as_ref())
                    .ok_or_else(|| Error::UnknownLayout(layout_name.clone()))?;

                for option in layout.options.iter().copied() {
                    let option_definition = &definition.options[option];
                    placements.push((
                        SettingPath::from_key(Arc::from(format!(
                            "ports.{}.{}.{index}.{}",
                            self.console, group_name, option_definition.key
                        ))),
                        option,
                    ));
                }
            }
        }

        for (path, option) in placements {
            ConfigurationInstance::append_setting(
                &self.definition,
                &mut self.settings,
                &mut self.setting_by_path,
                path,
                option,
            )?;
        }
        Ok(())
    }

    fn instantiate_input_options(&mut self) -> Result<()> {
        let Some(input) = self.definition.input.as_ref() else {
            return Ok(());
        };
        let placements = input
            .options
            .iter()
            .copied()
            .map(|option| {
                let key = &self.definition.options[option].key;
                (
                    SettingPath::from_key(Arc::from(format!("input.{key}"))),
                    option,
                )
            })
            .collect::<Vec<_>>();
        for (path, option) in placements {
            ConfigurationInstance::append_setting(
                &self.definition,
                &mut self.settings,
                &mut self.setting_by_path,
                path,
                option,
            )?;
        }
        Ok(())
    }

    fn finish(self) -> ConfigurationInstance {
        ConfigurationInstance {
            definition: self.definition,
            console: self.console,
            port_assignments: self.port_assignments,
            settings: self.settings,
            setting_by_path: self.setting_by_path,
            repeated_instances: IndexMap::new(),
        }
    }
}

impl ConfigurationInstance {
    pub fn definition(&self) -> &ConfigurationDefinition {
        &self.definition
    }

    pub fn console(&self) -> &str {
        &self.console
    }

    pub fn port_assignments(&self) -> &PortAssignments {
        &self.port_assignments
    }

    pub fn len(&self) -> usize {
        self.settings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.settings.is_empty()
    }

    pub fn handle(&self, path: &str) -> Option<Handle<Setting>> {
        self.setting_by_path.get(path).copied()
    }

    pub fn get(&self, path: &str) -> Option<&Value> {
        self.handle(path).map(|handle| &self[handle])
    }

    pub fn repeats(&self, path: &str) -> bool {
        self.section_for_instance_path(path)
            .is_some_and(|section| self.definition.sections[section].repeat)
    }

    pub fn append(&mut self, path: &str) -> Result<Box<str>> {
        let section = self
            .section_for_instance_path(path)
            .filter(|section| self.definition.sections[*section].repeat)
            .ok_or_else(|| Error::NotRepeatable(path.into()))?;
        if !self.has_valid_instance_ancestry(path) {
            return Err(Error::UnknownSetting(path.into()));
        }
        let index = self.repeated_instances.get(path).copied().unwrap_or(0);
        let prefix: Box<str> = format!("{path}.{index}").into();
        let options = self.definition.sections[section].options.clone();
        for option in options.iter().copied() {
            let setting = SettingPath::from_key(Arc::from(format!(
                "{prefix}.{}",
                self.definition.options[option].key
            )));
            Self::append_setting(
                &self.definition,
                &mut self.settings,
                &mut self.setting_by_path,
                setting,
                option,
            )?;
        }
        self.repeated_instances.insert(path.into(), index + 1);
        Ok(prefix)
    }

    pub(crate) fn repeated_instances(&self) -> &IndexMap<Box<str>, u32> {
        &self.repeated_instances
    }

    pub fn set_json(&mut self, path: &str, value: serde_json::Value) -> Result<()> {
        let handle = self
            .handle(path)
            .ok_or_else(|| Error::UnknownSetting(path.into()))?;
        let option = self.definition.option(self.settings[handle].option);
        let invalid = |expected| Error::InvalidSettingValue {
            path: path.into(),
            expected,
            actual: Self::json_kind(&value),
        };
        let parsed = if value.is_null() {
            Value::Null
        } else {
            match &option.value.kind {
                ValueKind::Boolean => value
                    .as_bool()
                    .map(Value::Boolean)
                    .ok_or_else(|| invalid("a boolean"))?,
                ValueKind::Integer { range } => {
                    let number = value.as_i64().ok_or_else(|| invalid("an integer"))?;
                    if let Some(OptionRange::Integer { min, max }) = range
                        && (number < *min || number > *max)
                    {
                        return Err(Error::OutOfRange {
                            path: path.into(),
                            value: number.to_string(),
                            min: min.to_string(),
                            max: max.to_string(),
                        });
                    }
                    Value::Integer(number)
                }
                ValueKind::Float { range } => {
                    let number = value
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| invalid("a finite number"))?;
                    if let Some(OptionRange::Float { min, max }) = range
                        && (number < *min || number > *max)
                    {
                        return Err(Error::OutOfRange {
                            path: path.into(),
                            value: number.to_string(),
                            min: min.to_string(),
                            max: max.to_string(),
                        });
                    }
                    Value::Float(number)
                }
                ValueKind::String => value
                    .as_str()
                    .map(|value| Value::String(value.into()))
                    .ok_or_else(|| invalid("a string"))?,
                ValueKind::Path => value
                    .as_str()
                    .map(|value| Value::Path(value.into()))
                    .ok_or_else(|| invalid("a path string"))?,
                ValueKind::Select { domain } => {
                    let choice = value.as_str().ok_or_else(|| invalid("a choice key"))?;
                    let known = match domain {
                        DomainDefinition::Static(choices) => choices
                            .iter()
                            .any(|candidate| candidate.key.as_str() == choice),
                        DomainDefinition::None => false,
                    };
                    if !known {
                        return Err(Error::UnknownChoice {
                            path: path.into(),
                            choice: choice.into(),
                        });
                    }
                    Value::Choice(choice.into())
                }
            }
        };
        self[handle] = parsed;
        Ok(())
    }

    pub fn setting(&self, path: &str) -> Option<&Setting> {
        let handle = self.handle(path)?;
        Some(&self.settings[handle])
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Handle<Setting>, &Setting)> {
        self.settings.iter()
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

    fn append_setting(
        definition: &ConfigurationDefinition,
        settings: &mut Arena<Setting>,
        setting_by_path: &mut HashMap<SettingPath, Handle<Setting>>,
        path: SettingPath,
        option: Handle<OptionDefinition>,
    ) -> Result<()> {
        if setting_by_path.contains_key(path.as_str()) {
            return Err(Error::DuplicateSetting(path));
        }
        let value = match &definition.options[option].value.default {
            DefaultDefinition::Literal(value) => value.clone(),
            DefaultDefinition::None => Value::Null,
        };
        let handle = settings.append(Setting {
            path: path.clone(),
            option,
            value,
        });
        setting_by_path.insert(path, handle);
        Ok(())
    }

    fn section_for_instance_path(&self, path: &str) -> Option<Handle<super::SectionDefinition>> {
        let definition_path = path
            .split('.')
            .filter(|component| component.parse::<u32>().is_err())
            .collect::<Vec<_>>()
            .join(".");
        self.definition
            .sections
            .fetch_if(|section| section.path.as_str() == definition_path)
    }

    fn has_valid_instance_ancestry(&self, path: &str) -> bool {
        let components = path.split('.').collect::<Vec<_>>();
        components.iter().enumerate().all(|(position, component)| {
            let Ok(index) = component.parse::<u32>() else {
                return true;
            };
            let parent = components[..position].join(".");
            self.repeated_instances
                .get(parent.as_str())
                .is_some_and(|count| index < *count)
        })
    }
}

impl ops::Index<Handle<Setting>> for ConfigurationInstance {
    type Output = Value;

    fn index(&self, handle: Handle<Setting>) -> &Self::Output {
        &self.settings[handle].value
    }
}

impl ops::IndexMut<Handle<Setting>> for ConfigurationInstance {
    fn index_mut(&mut self, handle: Handle<Setting>) -> &mut Self::Output {
        &mut self.settings[handle].value
    }
}

impl ops::Index<&str> for ConfigurationInstance {
    type Output = Value;

    fn index(&self, path: &str) -> &Self::Output {
        let handle = self.setting_by_path[path];
        &self[handle]
    }
}

impl ops::IndexMut<&str> for ConfigurationInstance {
    fn index_mut(&mut self, path: &str) -> &mut Self::Output {
        let handle = self.setting_by_path[path];
        &mut self[handle]
    }
}

impl<'a> IntoIterator for &'a ConfigurationInstance {
    type Item = &'a Setting;
    type IntoIter = slice::Iter<'a, Setting>;

    fn into_iter(self) -> Self::IntoIter {
        self.settings.values()
    }
}
