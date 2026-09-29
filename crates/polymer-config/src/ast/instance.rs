use std::{collections::BTreeMap, collections::HashMap, ops, slice, sync::Arc};

use indexmap::IndexMap;

use crate::{
    arena::{Arena, Handle},
    error::{Error, Result},
};

use super::{
    ConfigurationDefinition, DefaultDefinition, OptionDefinition, SettingPath, Value,
    ValueMismatch, family,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortAssignment {
    pub layout: Box<str>,
    pub mouse: bool,
}

pub type PortAssignments = BTreeMap<Box<str>, Box<[PortAssignment]>>;

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
        let mut instance = ConfigurationInstance {
            definition: self.clone(),
            console: console.into(),
            port_assignments,
            settings: Arena::new(),
            setting_by_path: HashMap::new(),
            repeated_instances: IndexMap::new(),
        };
        instance.instantiate_sections()?;
        instance.instantiate_input_options()?;
        instance.instantiate_ports()?;
        Ok(instance)
    }
}

impl ConfigurationInstance {
    fn instantiate_sections(&mut self) -> Result<()> {
        let definition = self.definition.clone();
        for section in definition.root_sections() {
            self.instantiate_section(&definition, section)?;
        }
        Ok(())
    }

    fn instantiate_section(
        &mut self,
        definition: &ConfigurationDefinition,
        handle: Handle<super::SectionDefinition>,
    ) -> Result<()> {
        let section = &definition.sections[handle];
        if section.repeat {
            return Ok(());
        }
        for option in section.options.iter().copied() {
            self.append_setting(
                section
                    .path
                    .join::<family::Setting>(&definition.options[option].key),
                option,
            )?;
        }
        for child in section.children.iter().copied() {
            self.instantiate_section(definition, child)?;
        }
        Ok(())
    }

    fn instantiate_ports(&mut self) -> Result<()> {
        let definition = self.definition.clone();
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

            for (index, assignment) in assignments.iter().enumerate() {
                let layout_name = &assignment.layout;
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
                    placements.push((
                        SettingPath::port(
                            &self.console,
                            group_name,
                            index,
                            &definition.options[option].key,
                        ),
                        option,
                    ));
                }
            }
        }

        for (path, option) in placements {
            self.append_setting(path, option)?;
        }
        Ok(())
    }

    fn instantiate_input_options(&mut self) -> Result<()> {
        let definition = self.definition.clone();
        let Some(input) = definition.input.as_ref() else {
            return Ok(());
        };
        for option in input.options.iter().copied() {
            self.append_setting(SettingPath::input(&definition.options[option].key), option)?;
        }
        Ok(())
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
            let setting = SettingPath::child(&prefix, &self.definition.options[option].key);
            self.append_setting(setting, option)?;
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
        let parsed = option
            .value
            .kind
            .parse_json(&value)
            .map_err(|mismatch| match mismatch {
                ValueMismatch::Kind(expected) => Error::InvalidSettingValue {
                    path: path.into(),
                    expected,
                    actual: Self::json_kind(&value),
                },
                ValueMismatch::Range { value, min, max } => Error::OutOfRange {
                    path: path.into(),
                    value,
                    min,
                    max,
                },
                ValueMismatch::Choice(choice) => Error::UnknownChoice {
                    path: path.into(),
                    choice,
                },
            })?;
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

    fn append_setting(&mut self, path: SettingPath, option: Handle<OptionDefinition>) -> Result<()> {
        if self.setting_by_path.contains_key(path.as_str()) {
            return Err(Error::DuplicateSetting(path));
        }
        let value = match &self.definition.options[option].value.default {
            DefaultDefinition::Literal(value) => value.clone(),
            DefaultDefinition::None => Value::Null,
        };
        let handle = self.settings.append(Setting {
            path: path.clone(),
            option,
            value,
        });
        self.setting_by_path.insert(path, handle);
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

impl<'a> IntoIterator for &'a ConfigurationInstance {
    type Item = &'a Setting;
    type IntoIter = slice::Iter<'a, Setting>;

    fn into_iter(self) -> Self::IntoIter {
        self.settings.values()
    }
}
