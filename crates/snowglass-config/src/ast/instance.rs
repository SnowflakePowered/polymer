use std::{collections::BTreeMap, collections::HashMap, ops, slice, sync::Arc};

use crate::arena::{Arena, Handle};

use super::{OptionDefinition, RealizedDefinition, SettingPath, Value, family};

pub type PortAssignments = BTreeMap<Box<str>, Box<[Box<str>]>>;

/// A launch-specific, flat collection of configuration values.
pub struct ConfigurationInstance {
    definition: Arc<RealizedDefinition>,
    console: Box<str>,
    port_assignments: PortAssignments,
    settings: Arena<Setting>,
    setting_by_path: HashMap<SettingPath, Handle<Setting>>,
}

/// A setting is deliberately ignorant of presentation and output concerns.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub path: SettingPath,
    pub value: Value,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InstantiateError {
    #[error("the definition has no input configuration")]
    NoInputDefinition,
    #[error("unknown console `{0}`")]
    UnknownConsole(Box<str>),
    #[error("unknown port group `{group}` for console `{console}`")]
    UnknownPortGroup { console: Box<str>, group: Box<str> },
    #[error(
        "port group `{group}` for console `{console}` expects at most {expected} assignments, got {actual}"
    )]
    TooManyPortAssignments {
        console: Box<str>,
        group: Box<str>,
        expected: usize,
        actual: usize,
    },
    #[error("port {index} is not declared in group `{group}`")]
    UnknownPort { group: Box<str>, index: usize },
    #[error("unknown input layout `{0}`")]
    UnknownLayout(Box<str>),
    #[error("port {index} in group `{group}` does not accept layout `{layout}`")]
    LayoutNotAccepted {
        group: Box<str>,
        index: usize,
        layout: Box<str>,
    },
    #[error("more than one setting materialized at `{0}`")]
    DuplicateSetting(SettingPath),
}

impl RealizedDefinition {
    pub fn instantiate(
        self: &Arc<Self>,
        console: impl Into<Box<str>>,
        port_assignments: PortAssignments,
    ) -> Result<ConfigurationInstance, InstantiateError> {
        let console = console.into();
        let mut builder = InstanceBuilder::new(self.clone(), console, port_assignments);
        builder.instantiate_sections()?;
        builder.instantiate_ports()?;
        Ok(builder.finish())
    }
}

struct InstanceBuilder {
    definition: Arc<RealizedDefinition>,
    console: Box<str>,
    port_assignments: PortAssignments,
    settings: Arena<Setting>,
    setting_by_path: HashMap<SettingPath, Handle<Setting>>,
}

impl InstanceBuilder {
    fn new(
        definition: Arc<RealizedDefinition>,
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

    fn instantiate_sections(&mut self) -> Result<(), InstantiateError> {
        let definition = self.definition.definition();
        let mut placements = Vec::new();
        for (_, section) in definition.sections.iter() {
            for option in section.options.iter().copied() {
                placements.push((
                    section
                        .path
                        .join::<family::Setting>(&definition.options[option].key),
                    option,
                ));
            }
        }

        for (path, option) in placements {
            self.append(path, option)?;
        }
        Ok(())
    }

    fn instantiate_ports(&mut self) -> Result<(), InstantiateError> {
        let definition = self.definition.definition();
        let Some(input) = definition.input.as_ref() else {
            return if self.port_assignments.is_empty() {
                Ok(())
            } else {
                Err(InstantiateError::NoInputDefinition)
            };
        };
        let platform = input
            .platforms
            .get(self.console.as_ref())
            .ok_or_else(|| InstantiateError::UnknownConsole(self.console.clone()))?;

        let mut placements = Vec::new();
        for (group_name, assignments) in &self.port_assignments {
            let group = platform.groups.get(group_name.as_ref()).ok_or_else(|| {
                InstantiateError::UnknownPortGroup {
                    console: self.console.clone(),
                    group: group_name.clone(),
                }
            })?;
            let port_count = group
                .ports
                .iter()
                .map(|port| port.index + 1)
                .max()
                .unwrap_or(0);
            if assignments.len() > port_count {
                return Err(InstantiateError::TooManyPortAssignments {
                    console: self.console.clone(),
                    group: group_name.clone(),
                    expected: port_count,
                    actual: assignments.len(),
                });
            }

            for (index, layout_name) in assignments.iter().enumerate() {
                let port = group
                    .ports
                    .iter()
                    .find(|port| port.index == index)
                    .ok_or_else(|| InstantiateError::UnknownPort {
                        group: group_name.clone(),
                        index,
                    })?;
                if !port.layouts.contains(layout_name) {
                    return Err(InstantiateError::LayoutNotAccepted {
                        group: group_name.clone(),
                        index,
                        layout: layout_name.clone(),
                    });
                }
                let layout = input
                    .layouts
                    .get(layout_name.as_ref())
                    .ok_or_else(|| InstantiateError::UnknownLayout(layout_name.clone()))?;

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
            self.append(path, option)?;
        }
        Ok(())
    }

    fn append(
        &mut self,
        path: SettingPath,
        option: Handle<OptionDefinition>,
    ) -> Result<(), InstantiateError> {
        if self.setting_by_path.contains_key(path.as_str()) {
            return Err(InstantiateError::DuplicateSetting(path));
        }

        let value = self.definition.realized_option(option).default.clone();
        let handle = self.settings.append(Setting {
            path: path.clone(),
            value,
        });
        self.setting_by_path.insert(path, handle);
        Ok(())
    }

    fn finish(self) -> ConfigurationInstance {
        ConfigurationInstance {
            definition: self.definition,
            console: self.console,
            port_assignments: self.port_assignments,
            settings: self.settings,
            setting_by_path: self.setting_by_path,
        }
    }
}

impl ConfigurationInstance {
    pub fn definition(&self) -> &RealizedDefinition {
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

    pub fn get_mut(&mut self, path: &str) -> Option<&mut Value> {
        let handle = self.handle(path)?;
        Some(&mut self[handle])
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Handle<Setting>, &Setting)> {
        self.settings.iter()
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (Handle<Setting>, &mut Setting)> {
        self.settings.iter_mut()
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
