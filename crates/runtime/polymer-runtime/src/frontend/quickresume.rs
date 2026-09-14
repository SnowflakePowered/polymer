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
            if let Some(disc) = argument["launchSet"]
                .get(entry.disc_index as usize)
                .cloned()
            {
                argument["launchPath"] = disc["launchPath"].clone();
                argument["package"] = disc["package"].clone();
            }
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
        let anchor = argument["launchSet"]
            .as_array()
            .and_then(|set| set.first())
            .unwrap_or(argument);
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
                anchor["launchPath"].as_str().map(|path| {
                    std::fs::canonicalize(path)
                        .unwrap_or_else(|_| std::path::PathBuf::from(path))
                        .to_string_lossy()
                        .to_ascii_lowercase()
                })
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
                        disc["launchPath"].clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_identity_depends_on_disc_order_and_contents_not_initial_disc() {
        let mut launch = serde_json::json!({
            "initialDiscIndex": 0,
            "launchSet": [
                { "launchPath": "disc0.iso", "package": { "filename": "disc0.iso", "platform": "PSX", "serial": "0", "titleId": null, "binaryDigest": "aaa" } },
                { "launchPath": "disc1.iso", "package": { "filename": "disc1.iso", "platform": "PSX", "serial": "1", "titleId": null, "binaryDigest": "bbb" } },
            ],
        });
        let key = QuickResume::key("test", &launch).unwrap();
        launch["initialDiscIndex"] = 1.into();
        assert_eq!(QuickResume::key("test", &launch), Some(key.clone()));
        launch["launchSet"].as_array_mut().unwrap().swap(0, 1);
        assert_ne!(QuickResume::key("test", &launch), Some(key.clone()));
        launch["launchSet"].as_array_mut().unwrap().swap(0, 1);
        launch["launchSet"][1]["package"]["binaryDigest"] = "changed".into();
        assert_ne!(QuickResume::key("test", &launch), Some(key));
    }
    #[test]
    fn single_disc_resume_changes_when_its_manifest_changes() {
        let mut launch = serde_json::json!({ "launchSet": [{
            "launchPath": "disc.cue", "package": {
                "filename": "disc.cue", "platform": "SONY_PSX", "serial": "SLUS-00000",
                "titleId": null, "binaryDigest": "unchanged",
                "bundle": { "revision": "first", "entrypoint": "disc.cue", "files": [] }
            }
        }] });
        let before = QuickResume::key("test", &launch).unwrap();
        launch["launchSet"][0]["package"]["bundle"]["revision"] = "new-audio".into();
        assert_ne!(QuickResume::key("test", &launch), Some(before));
    }
}
