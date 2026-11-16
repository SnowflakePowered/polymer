use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use polymer_config::{
    ast::{ConfigurationInstance, Value},
    ser::{GamepadBinding, RenderOptions, Serializer},
};
use serde::Serialize;
use serde_bytes::ByteBuf;
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
        topology: &polymer_controllers::ControllerTopology,
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
        topology: &polymer_controllers::ControllerTopology,
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
        options: &RenderOptions,
    ) -> Result<SerializedConfiguration, ConfigurationAccessError> {
        let entry = self
            .instances
            .get(&id)
            .ok_or(ConfigurationAccessError::UnknownConfiguration(id))?;
        serialize(&entry.instance, entry.bindings.clone(), options)
    }

    fn bindings(topology: &polymer_controllers::ControllerTopology) -> Vec<GamepadBinding> {
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
    options: &RenderOptions,
) -> Result<SerializedConfiguration, ConfigurationAccessError> {
    let serializer = Serializer::new(instance, bindings)?;
    let files = serializer.serialize(options)?;
    Ok(files
        .into_iter()
        .map(|(path, bytes)| (path.into(), bytes))
        .collect())
}

#[derive(Debug, Error)]
pub enum ConfigurationAccessError {
    #[error("plugin configuration invocation {0} does not exist")]
    UnknownConfiguration(ConfigurationId),
    #[error(transparent)]
    Configuration(#[from] polymer_config::error::Error),
}

#[op2]
#[serde]
fn op_configuration_get(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
) -> Result<ConfigurationLookup, JsErrorBox> {
    let value = state
        .borrow::<ConfigurationInstances>()
        .get(id, &path)
        .map_err(|error| JsErrorBox::generic(error.to_string()))?;
    Ok(match value {
        Some(value) => ConfigurationLookup { found: true, value },
        None => ConfigurationLookup {
            found: false,
            value: serde_json::Value::Null,
        },
    })
}

#[op2]
fn op_configuration_set(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
    #[serde] value: serde_json::Value,
) -> Result<(), JsErrorBox> {
    state
        .borrow_mut::<ConfigurationInstances>()
        .set(id, &path, value)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
fn op_configuration_repeats(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
) -> Result<bool, JsErrorBox> {
    state
        .borrow::<ConfigurationInstances>()
        .repeats(id, &path)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[string]
fn op_configuration_append(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
) -> Result<String, JsErrorBox> {
    state
        .borrow_mut::<ConfigurationInstances>()
        .append(id, &path)
        .map(Into::into)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[serde]
fn op_configuration_entries(
    state: &mut OpState,
    id: ConfigurationId,
) -> Result<Vec<(String, serde_json::Value)>, JsErrorBox> {
    state
        .borrow::<ConfigurationInstances>()
        .entries(id)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[serde]
fn op_configuration_render(
    state: &mut OpState,
    id: ConfigurationId,
    #[serde] options: Option<RenderOptions>,
) -> Result<BTreeMap<String, ByteBuf>, JsErrorBox> {
    Ok(state
        .borrow_mut::<ConfigurationInstances>()
        .render(id, &options.unwrap_or_default())
        .map_err(|error| JsErrorBox::generic(error.to_string()))?
        .into_iter()
        .map(|(path, bytes)| (path, ByteBuf::from(bytes)))
        .collect())
}

#[op2]
#[serde]
fn op_configuration_actions(
    state: &mut OpState,
    id: ConfigurationId,
) -> Result<BTreeMap<String, String>, JsErrorBox> {
    state
        .borrow::<ConfigurationInstances>()
        .actions(id)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigurationLookup {
    found: bool,
    value: serde_json::Value,
}

deno_core::extension!(
    polymer_configuration,
    ops = [
        op_configuration_get,
        op_configuration_set,
        op_configuration_repeats,
        op_configuration_append,
        op_configuration_entries,
        op_configuration_render,
        op_configuration_actions,
    ],
    esm_entry_point = "ext:polymer_configuration/00_configuration.js",
    esm = [dir "configuration/js", "00_configuration.js"],
    state = |state| state.put(ConfigurationInstances::default()),
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_configuration::init()
}
