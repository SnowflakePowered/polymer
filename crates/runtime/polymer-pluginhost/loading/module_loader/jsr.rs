use super::{JSR_ORIGIN, PluginModuleLoader};
use deno_core::{
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, error::ModuleLoaderError,
};

impl PluginModuleLoader {
    pub(super) fn resolve_jsr(
        &self,
        specifier: &str,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let value = specifier
            .strip_prefix("jsr:")
            .expect("checked JSR specifier");
        let parts = value.split('/').collect::<Vec<_>>();
        if parts.len() < 2
            || !parts[0].starts_with('@')
            || parts[0].len() < 2
            || parts[1].is_empty()
        {
            return Err(ModuleLoaderError::type_error(format!(
                "invalid JSR specifier '{specifier}'"
            )));
        }
        let (name, version) = parts[1]
            .rsplit_once('@')
            .filter(|(name, version)| !name.is_empty() && !version.is_empty())
            .map_or((parts[1], None), |(name, version)| (name, Some(version)));
        let package = format!("{}/{name}", parts[0]);
        let export = if parts.len() == 2 {
            ".".to_owned()
        } else {
            format!("./{}", parts[2..].join("/"))
        };
        if let Some(directory) = self.jsr_overrides.get(&package) {
            return Self::resolve_local(&package, &export, directory.value());
        }
        let package =
            percent_encoding::utf8_percent_encode(&package, percent_encoding::NON_ALPHANUMERIC);
        let export =
            percent_encoding::utf8_percent_encode(&export, percent_encoding::NON_ALPHANUMERIC);
        let version = version.map(|version| {
            percent_encoding::utf8_percent_encode(version, percent_encoding::NON_ALPHANUMERIC)
        });
        ModuleSpecifier::parse(&format!(
            "{JSR_ORIGIN}/.polymer-resolve?package={package}&export={export}{}",
            version
                .map(|version| format!("&version={version}"))
                .unwrap_or_default()
        ))
        .map_err(ModuleLoaderError::from_err)
    }

    /// Resolves an export of a package from its local `jsr.json`.
    fn resolve_local(
        package: &str,
        export: &str,
        directory: &std::path::Path,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join("jsr.json")).map_err(ModuleLoaderError::from_err)?,
        )
        .map_err(ModuleLoaderError::from_err)?;
        let path = match manifest.get("exports") {
            Some(serde_json::Value::String(path)) if export == "." => Some(path.as_str()),
            Some(serde_json::Value::Object(exports)) => {
                exports.get(export).and_then(serde_json::Value::as_str)
            }
            _ => None,
        }
        .ok_or_else(|| {
            ModuleLoaderError::type_error(format!(
                "JSR package '{package}' has no export '{export}'"
            ))
        })?;
        ModuleSpecifier::from_file_path(directory.join(path.trim_start_matches("./"))).map_err(
            |()| {
                ModuleLoaderError::type_error(format!(
                    "cannot resolve local JSR package '{package}'"
                ))
            },
        )
    }

    fn jsr_cache(&self) -> Result<&std::path::Path, ModuleLoaderError> {
        self.jsr_cache.as_deref().map(AsRef::as_ref).ok_or_else(|| {
            ModuleLoaderError::type_error("the Polymer JSR cache directory is unavailable")
        })
    }

    pub(super) async fn cached_get(&self, url: &str) -> Result<Vec<u8>, ModuleLoaderError> {
        let root = self.jsr_cache()?;
        let name = blake3::hash(url.as_bytes()).to_hex().to_string();
        let path = root.join(name);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let mut permissions = polymer_deno::Runtime::network_permissions(&self.network_hosts)
            .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?;
        let target = ModuleSpecifier::parse(url).map_err(ModuleLoaderError::from_err)?;
        permissions
            .check_net_url(&target, "JSR import")
            .map_err(ModuleLoaderError::from_err)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ModuleLoaderError::generic(error.to_string()))?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| ModuleLoaderError::generic(error.to_string()))?
            .error_for_status()
            .map_err(|error| ModuleLoaderError::generic(error.to_string()))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|error| ModuleLoaderError::generic(error.to_string()))?
            .to_vec();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(ModuleLoaderError::from_err)?;
        let temporary = path.with_extension("tmp");
        tokio::fs::write(&temporary, &bytes)
            .await
            .map_err(ModuleLoaderError::from_err)?;
        if let Err(error) = tokio::fs::rename(&temporary, &path).await {
            if !path.exists() {
                return Err(ModuleLoaderError::from_err(error));
            }
            let _ = tokio::fs::remove_file(temporary).await;
        }
        Ok(bytes)
    }

    pub(super) async fn load_jsr(
        &self,
        specifier: ModuleSpecifier,
        module_type: ModuleType,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        if specifier.path() == "/.polymer-resolve" {
            let package = specifier
                .query_pairs()
                .find(|(key, _)| key == "package")
                .map(|(_, value)| value.into_owned())
                .ok_or_else(|| ModuleLoaderError::type_error("JSR package is missing"))?;
            let export = specifier
                .query_pairs()
                .find(|(key, _)| key == "export")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_else(|| ".".to_owned());
            let requested_version = specifier
                .query_pairs()
                .find(|(key, _)| key == "version")
                .map(|(_, value)| value.into_owned());
            // Exact versions have immutable metadata of their own. Do not reject
            // a published version because the cached package index predates it.
            let version = if let Some(version) = requested_version
                .as_deref()
                .and_then(|value| semver::Version::parse(value).ok())
            {
                version.to_string()
            } else {
                let meta_url = format!("{JSR_ORIGIN}/{package}/meta.json");
                let meta: serde_json::Value =
                    serde_json::from_slice(&self.cached_get(&meta_url).await?)
                        .map_err(ModuleLoaderError::from_err)?;
                match requested_version {
                    None => meta
                        .get("latest")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    Some(requested)
                        if meta
                            .get("versions")
                            .and_then(|versions| versions.get(&requested))
                            .is_some() =>
                    {
                        Some(requested)
                    }
                    Some(requested) => {
                        let requirement =
                            semver::VersionReq::parse(&requested).map_err(|error| {
                                ModuleLoaderError::type_error(format!(
                                    "invalid JSR version requirement '{requested}': {error}"
                                ))
                            })?;
                        meta.get("versions")
                            .and_then(serde_json::Value::as_object)
                            .into_iter()
                            .flatten()
                            .filter_map(|(version, _)| semver::Version::parse(version).ok())
                            .filter(|version| requirement.matches(version))
                            .max()
                            .map(|version| version.to_string())
                    }
                }
                .ok_or_else(|| {
                    ModuleLoaderError::type_error(format!(
                        "JSR package '{package}' has no matching version"
                    ))
                })?
            };
            let version_url = format!("{JSR_ORIGIN}/{package}/{version}_meta.json");
            let version_meta: serde_json::Value =
                serde_json::from_slice(&self.cached_get(&version_url).await?)
                    .map_err(ModuleLoaderError::from_err)?;
            let path = version_meta
                .get("exports")
                .and_then(|exports| exports.get(&export))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ModuleLoaderError::type_error(format!(
                        "JSR package '{package}' has no export '{export}'"
                    ))
                })?
                .trim_start_matches('.');
            let found = ModuleSpecifier::parse(&format!("{JSR_ORIGIN}/{package}/{version}{path}"))
                .map_err(ModuleLoaderError::from_err)?;
            let bytes = self.cached_get(found.as_str()).await?;
            let code = self.transform_source(&found, bytes)?;
            return Ok(ModuleSource::new_with_redirect(
                module_type,
                ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
                &specifier,
                &found,
                None,
            ));
        }
        let bytes = self.cached_get(specifier.as_str()).await?;
        let code = self.transform_source(&specifier, bytes)?;
        Ok(ModuleSource::new(
            module_type,
            ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
            &specifier,
            None,
        ))
    }
}
