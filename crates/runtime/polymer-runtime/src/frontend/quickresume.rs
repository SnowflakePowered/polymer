use polymer_pluginhost::emulator::EmulatorPluginInstance;
use serde_json::Value;

pub(super) struct QuickResume;

impl QuickResume {
    pub(super) async fn prepare(
        plugins: &polymer_pluginhost::runtime::PluginRuntimeHandle,
        plugin_id: &str,
        path: &[String],
        arguments: &mut [Value],
    ) -> (
        Option<polymer_pluginhost::emulator::QuickResumeData>,
        Option<Value>,
    ) {
        if !matches!(path, [name] if name == "onLaunch")
            && !matches!(path, [root, name] if root == "default" && name == "onLaunch")
        {
            return (None, None);
        }
        let Some(plugin) = plugins
            .registry()
            .plugin::<dyn EmulatorPluginInstance>(plugin_id)
        else {
            return (None, None);
        };
        let Some(revision) = plugin
            .save_states()
            .and_then(|states| states.revision.as_deref())
            .map(str::to_owned)
        else {
            return (None, None);
        };
        let Some(argument) = arguments.first_mut() else {
            return (None, None);
        };
        let Some(key) = Self::key(plugin_id, argument) else {
            return (None, None);
        };
        let cache = serde_json::json!({ "key": key.as_str(), "revision": revision });
        let loaded = tokio::task::spawn_blocking({
            let key = key.clone();
            let revision = revision.clone();
            move || polymer_library::QuickResumeStore::discover()?.take(&key, &revision)
        })
        .await;
        let entry = match loaded {
            Ok(Ok(entry)) => entry,
            Ok(Err(error)) => {
                tracing::warn!(
                    plugin = plugin_id,
                    "polymer: loading Quick Resume failed: {error}"
                );
                None
            }
            Err(error) => {
                tracing::warn!(
                    plugin = plugin_id,
                    "polymer: Quick Resume task failed: {error}"
                );
                None
            }
        };
        let entry = entry.filter(|entry| {
            (entry.disc_index as usize) < argument["launchSet"].as_array().map_or(1, Vec::len)
        });
        if let Some(entry) = &entry {
            argument["initialDiscIndex"] = entry.disc_index.into();
        }
        (
            entry.map(|entry| polymer_pluginhost::emulator::QuickResumeData {
                state: entry.state,
                screenshot: entry.screenshot,
            }),
            Some(cache),
        )
    }

    fn key(plugin_id: &str, argument: &Value) -> Option<polymer_library::QuickResumeKey> {
        let anchor = argument["launchSet"].as_array()?.first()?;
        let Ok(package) =
            serde_json::from_value::<polymer_scraping::Package>(anchor["package"].clone())
        else {
            return None;
        };
        let platform = package.platform.as_str();
        let mut identity = package
            .title_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                package
                    .serial
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
            .map(|value| value.trim().to_ascii_lowercase())
            .or_else(|| {
                let path = package.uri.to_string();
                Some(
                    std::fs::canonicalize(&path)
                        .unwrap_or_else(|_| std::path::PathBuf::from(&path))
                        .to_string_lossy()
                        .to_ascii_lowercase(),
                )
            })
            .unwrap_or_default();
        // An index is only meaningful for this exact ordered set of binaries and paths.
        if let Some(set) = argument["launchSet"].as_array().filter(|set| {
            set.len() > 1 || set.iter().any(|disc| disc["package"]["bundle"].is_object())
        }) {
            let identities: Vec<_> = set
                .iter()
                .map(|disc| {
                    let mut parts = vec![
                        serde_json::from_value::<polymer_library::LaunchableUri>(
                            disc["package"]["uri"].clone(),
                        )
                        .map_or(Value::Null, |uri| uri.to_string().into()),
                        disc["package"]["binaryDigest"].clone(),
                    ];
                    if let Some(revision) = disc["package"]["bundle"].get("revision") {
                        parts.push(revision.clone());
                    }
                    serde_json::json!(parts)
                })
                .collect();
            identity = serde_json::to_string(&identities).expect("launch identities are JSON");
        }
        Some(polymer_library::QuickResumeKey::new(
            plugin_id, platform, &identity,
        ))
    }
}
