use super::{DigestAlgorithm, Digestible, PackageBinary};
use deno_core::OpState;
use deno_error::JsErrorBox;
use polymer_deno::digest::{DigestFuture, DigestHandler, DigestSource};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Default)]
pub(crate) struct DigestState {
    sources: BTreeMap<String, (PathBuf, bool)>,
}

impl DigestState {
    /// Register only package/file descriptors supplied at host API boundaries.
    pub(crate) fn package(&mut self, value: &mut Value) {
        if value.get("uri").is_none() {
            return;
        }
        if let Some(files) = value.get_mut("files").and_then(Value::as_array_mut) {
            for file in files {
                self.register(file, false);
            }
        }
        self.register(value, true);
    }

    fn register(&mut self, value: &mut Value, package: bool) {
        let Some(values) = value.as_object_mut() else {
            return;
        };
        let Some(path) = values
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|uri| uri.parse::<polymer_library::LaunchableUri>().ok())
            .and_then(|uri| uri.binary_path().ok())
        else {
            return;
        };
        let token = self.sources.len().to_string();
        self.sources.insert(token.clone(), (path, package));
        values.insert("__digestToken".into(), token.into());
        values.remove("binaryDigest");
        values.remove("__manifest");
    }

    pub(crate) fn launch(&mut self, value: &mut Value) {
        if let Some(package) = value.get_mut("package") {
            self.package(package);
        }
        if let Some(discs) = value.get_mut("launchSet").and_then(Value::as_array_mut) {
            for disc in discs {
                if let Some(package) = disc.get_mut("package") {
                    self.package(package);
                }
            }
        }
    }

    pub(crate) fn arguments(&mut self, arguments: &mut [Value]) {
        for argument in arguments {
            self.package(argument);
            self.launch(argument);
        }
    }

    fn source(&self, token: &str, binary: bool) -> Result<PathBuf, JsErrorBox> {
        let (path, package) = self
            .sources
            .get(token)
            .ok_or_else(|| JsErrorBox::type_error("invalid digest source"))?;
        if binary && !package {
            return Err(JsErrorBox::type_error("file has no normalized binary"));
        }
        Ok(path.clone())
    }

    fn digest(
        state: &mut OpState,
        source: DigestSource,
        algorithm: String,
    ) -> Result<DigestFuture, JsErrorBox> {
        let algorithm: DigestAlgorithm =
            serde_json::from_value(Value::String(algorithm)).map_err(JsErrorBox::from_err)?;
        let source = match source {
            DigestSource::Handle { token, binary } => {
                let path = state.borrow::<Self>().source(&token, binary)?;
                if binary {
                    ResolvedSource::Binary(path)
                } else {
                    ResolvedSource::Path(path)
                }
            }
            DigestSource::Cache { key, path } => ResolvedSource::Path(
                state
                    .borrow::<crate::cache::CacheState>()
                    .path(&key, &path)?,
            ),
            DigestSource::Resource {
                kind,
                token,
                components,
            } => ResolvedSource::File(
                state
                    .borrow::<crate::filesystem::FilesystemState>()
                    .resource_file(&kind, &token, &components)
                    .map_err(|error| JsErrorBox::generic(error.to_string()))?,
            ),
            DigestSource::Uri { .. } => {
                return Err(JsErrorBox::type_error(
                    "plugins cannot digest arbitrary host paths",
                ));
            }
        };
        Ok(Box::pin(async move {
            tokio::task::spawn_blocking(move || source.digest(algorithm))
                .await
                .map_err(JsErrorBox::from_err)?
                .map_err(JsErrorBox::from_err)
        }))
    }
}

enum ResolvedSource {
    Path(PathBuf),
    Binary(PathBuf),
    File(Arc<dyn polymer_vfs::FileNode>),
}

impl Digestible for ResolvedSource {
    fn digest(&self, algorithm: DigestAlgorithm) -> std::io::Result<String> {
        match self {
            Self::Path(path) => path.as_path().digest(algorithm),
            Self::Binary(path) => PackageBinary(path).digest(algorithm),
            Self::File(file) => file.as_ref().digest(algorithm),
        }
    }
}

deno_core::extension!(polymer_digest,
    esm = [dir "digest/js", "00_digest.js"],
    state = |state| {
        state.put(DigestState::default());
        state.put(DigestHandler(DigestState::digest));
    },
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_digest::init()
}
