use snowglass_config::{
    ast::{ConfigurationInstance, Value},
    ser::{BmlEmitter, Emitter, GamepadBinding, IniEmitter, Serializer, XmlEmitter},
};
use std::collections::BTreeMap;
use thiserror::Error;

pub type SerializedConfiguration = BTreeMap<String, Vec<u8>>;

pub(crate) type ConfigurationId = u32;

/// Invocation-scoped configurations exposed to plugin JavaScript. Proxies
/// carry their ID into every op, so no ambient active invocation is required.
#[derive(Default)]
pub(crate) struct ConfigurationInstances {
    next_id: ConfigurationId,
    instances: BTreeMap<ConfigurationId, ConfigurationEntry>,
}

#[derive(Clone)]
struct ConfigurationEntry {
    instance: ConfigurationInstance,
    bindings: Vec<GamepadBinding>,
}

pub(crate) struct ConfigurationCheckpoint(ConfigurationEntry);

impl ConfigurationInstances {
    pub(crate) fn actions(
        &self,
        id: ConfigurationId,
    ) -> Result<BTreeMap<String, String>, ConfigurationAccessError> {
        let entry = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        Ok(entry
            .instance
            .definition()
            .actions
            .as_ref()
            .map(|actions| {
                actions
                    .map
                    .iter()
                    .map(|(name, action)| (name.to_string(), action.expression.to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub(crate) fn insert(
        &mut self,
        instance: ConfigurationInstance,
        topology: &snowglass_controllers::ControllerTopology,
    ) -> ConfigurationId {
        let id = loop {
            self.next_id = self.next_id.wrapping_add(1);
            if self.next_id == 0 {
                continue;
            }
            if !self.instances.contains_key(&self.next_id) {
                break self.next_id;
            }
        };
        self.instances.insert(
            id,
            ConfigurationEntry {
                instance,
                bindings: Self::bindings(topology),
            },
        );
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
        Ok(instance.instance.get(path).map(Value::to_json))
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
        instance.instance.set_json(path, value)?;
        Ok(())
    }

    pub(crate) fn repeats(
        &self,
        id: ConfigurationId,
        path: &str,
    ) -> Result<bool, ConfigurationAccessError> {
        let entry = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        Ok(entry.instance.repeats(path))
    }

    pub(crate) fn append(
        &mut self,
        id: ConfigurationId,
        path: &str,
    ) -> Result<Box<str>, ConfigurationAccessError> {
        let entry = self
            .instances
            .get_mut(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        Ok(entry.instance.append(path)?)
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
            .instance
            .iter()
            .map(|(_, setting)| (setting.path.as_str().to_owned(), setting.value.to_json()))
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn take(
        &mut self,
        id: ConfigurationId,
    ) -> Result<ConfigurationInstance, ConfigurationAccessError> {
        self.instances
            .remove(&id)
            .map(|entry| entry.instance)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))
    }

    pub(crate) fn discard(&mut self, id: ConfigurationId) {
        self.instances.remove(&id);
    }

    pub(crate) fn checkpoint(
        &self,
        id: ConfigurationId,
    ) -> Result<ConfigurationCheckpoint, ConfigurationAccessError> {
        self.instances
            .get(&id)
            .cloned()
            .map(ConfigurationCheckpoint)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))
    }

    pub(crate) fn restore(&mut self, id: ConfigurationId, checkpoint: ConfigurationCheckpoint) {
        self.instances.insert(id, checkpoint.0);
    }

    pub(crate) fn rematerialize(
        &mut self,
        id: ConfigurationId,
        mut replacement: ConfigurationInstance,
        topology: &snowglass_controllers::ControllerTopology,
    ) -> Result<(), ConfigurationAccessError> {
        let current = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        let values = current
            .instance
            .iter()
            .map(|(_, setting)| (setting.path.as_str().to_owned(), setting.value.to_json()))
            .collect::<Vec<_>>();
        for (path, value) in values {
            if replacement.setting(&path).is_some() {
                replacement.set_json(&path, value)?;
            }
        }
        self.instances.insert(
            id,
            ConfigurationEntry {
                instance: replacement,
                bindings: Self::bindings(topology),
            },
        );
        Ok(())
    }

    pub(crate) fn render(
        &mut self,
        id: ConfigurationId,
    ) -> Result<SerializedConfiguration, ConfigurationAccessError> {
        let entry = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        serialize(&entry.instance, entry.bindings.clone())
    }

    fn bindings(topology: &snowglass_controllers::ControllerTopology) -> Vec<GamepadBinding> {
        topology
            .mappings
            .iter()
            .map(|mapping| GamepadBinding {
                group: mapping.target.group.clone().into_boxed_str(),
                port: mapping.target.port,
                gamepad: usize::from(mapping.gamepad),
            })
            .collect()
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

fn emitter_for(format: &str) -> Option<Box<dyn Emitter>> {
    match format {
        "bml" => Some(Box::new(BmlEmitter::new())),
        "ini" => Some(Box::new(IniEmitter::new())),
        "xml" => Some(Box::new(XmlEmitter::new())),
        _ => None,
    }
}

#[derive(Debug, Error)]
pub enum ConfigurationAccessError {
    #[error("plugin configuration invocation {0} does not exist")]
    UnknownConfiguration(ConfigurationId),
    #[error(transparent)]
    Configuration(#[from] snowglass_config::error::Error),
}
