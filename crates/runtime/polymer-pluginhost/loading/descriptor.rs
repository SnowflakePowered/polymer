use crate::plugin::PluginInstance;
use serde_json::Value;

/// Frontend-safe metadata for one loaded plugin.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDescriptor {
    pub id: String,
    pub version: String,
    pub class: String,
    pub caches: Vec<PluginCacheDescriptor>,
    pub config: Vec<PluginOptionDescriptor>,
}

/// A plugin-local persistent cache declared in `plugin.toml`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCacheDescriptor {
    pub key: String,
    pub label: String,
}

/// A `[plugin.config]` option with a `display` name.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginOptionDescriptor {
    pub key: String,
    pub display: String,
    pub default: Value,
    pub choices: Vec<Value>,
}

impl PluginDescriptor {
    pub(crate) fn from_instance(plugin: &dyn PluginInstance) -> Self {
        Self {
            id: plugin.id().to_owned(),
            version: plugin.version().to_owned(),
            class: plugin.class().to_owned(),
            caches: plugin
                .caches()
                .iter()
                .map(|cache| PluginCacheDescriptor {
                    key: cache.key.clone(),
                    label: cache.label.clone(),
                })
                .collect(),
            config: plugin
                .config()
                .options()
                .map(|(key, option)| PluginOptionDescriptor {
                    key: key.to_owned(),
                    display: option.display.clone(),
                    default: option.default_value(),
                    choices: option.choice_values(),
                })
                .collect(),
        }
    }
}
