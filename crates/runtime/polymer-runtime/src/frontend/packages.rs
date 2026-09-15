use polymer_pluginhost::digest::{DigestAlgorithm, Digestible, PackageBinary};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

/// Direct plugin launches can receive a lazily identified package without going
/// through configuration preparation. Fill host identity before resume/storage
/// selection; the plugin argument binder still hides it from JS.
#[derive(Default)]
pub(super) struct LaunchPackages {
    identities: BTreeMap<PathBuf, String>,
}

impl LaunchPackages {
    pub(super) async fn prepare(mut arguments: Vec<Value>) -> Result<Vec<Value>, String> {
        tokio::task::spawn_blocking(move || {
            let mut packages = Self::default();
            for argument in &mut arguments {
                packages.bind(argument)?;
            }
            Ok::<_, std::io::Error>(arguments)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
    }

    fn bind(&mut self, value: &mut Value) -> Result<(), std::io::Error> {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.bind(value)?;
                }
            }
            Value::Object(values) => {
                if values.get("binaryDigest").and_then(Value::as_str) == Some("")
                    && let Some(path) = values.get("__digestPath").and_then(Value::as_str)
                {
                    let digest = self.identity(PathBuf::from(path))?;
                    values.insert("binaryDigest".into(), digest.into());
                }
                for value in values.values_mut() {
                    self.bind(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn identity(&mut self, path: PathBuf) -> Result<String, std::io::Error> {
        let digest = match self.identities.entry(path) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let digest = PackageBinary(entry.key()).digest(DigestAlgorithm::Blake3)?;
                entry.insert(digest)
            }
        };
        Ok(digest.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_launch_resolves_lazy_identity_and_preserves_registered_packages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.nes");
        let mut bytes = vec![0; 16];
        bytes[..4].copy_from_slice(b"NES\x1a");
        bytes.extend_from_slice(b"battery save identity");
        std::fs::write(&path, bytes).unwrap();
        let expected = blake3::hash(b"battery save identity").to_hex().to_string();
        let lazy = serde_json::json!({ "__digestPath": path, "binaryDigest": "" });
        let registered =
            serde_json::json!({ "__digestPath": "missing.nes", "binaryDigest": "existing" });
        let result = LaunchPackages::prepare(vec![serde_json::json!({
            "package": lazy, "launchSet": [{"package": lazy}, {"package": registered}]
        })])
        .await
        .unwrap();
        assert_eq!(result[0]["package"]["binaryDigest"], expected);
        assert_eq!(
            result[0]["launchSet"][0]["package"]["binaryDigest"],
            expected
        );
        assert_eq!(result[0]["launchSet"][1]["package"], registered);
    }
}
