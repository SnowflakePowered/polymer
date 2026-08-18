use crate::runtime::PluginInstance;

/// Frontend-safe metadata for one loaded plugin.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDescriptor {
    pub id: String,
    pub version: String,
    pub class: String,
    pub caches: Vec<PluginCacheDescriptor>,
}

/// A plugin-local persistent cache declared in `plugin.toml`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCacheDescriptor {
    pub key: String,
    pub label: String,
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
        }
    }
}
