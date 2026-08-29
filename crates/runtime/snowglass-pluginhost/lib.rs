//! Isolated Snowglass plugin registry and runtime.
//!
//! Package discovery is kept separate from execution. Every call made through
//! [`PluginRuntimeHandle`] runs in a fresh curated Deno/V8 isolate owned by this
//! crate, and invocation-scoped resources are destroyed with that isolate.

#![forbid(unsafe_code)]

mod configuration;
mod controller;
mod deno;
pub mod descriptor;
mod emulator;
mod filesystem;
mod handle;
mod module_loader;
mod plugin_runtime;
mod runtime;
pub mod toml;

pub use configuration::{ConfigurationAccessError, SerializedConfiguration};
pub use controller::ControllerSession;
pub use emulator::{
    EmulatorHostError, EmulatorLaunchArguments, EmulatorPluginInstance, EmulatorPort,
    EmulatorSetupInvocation, GraphicsHook, TrackedSurface,
};
pub use filesystem::{StorageFactories, StorageFactory};
pub use module_loader::PluginModuleLoader;
pub use plugin_runtime::{
    PluginCancellationToken, PluginInvocationError, PluginInvocationResult, PluginRuntime,
    PluginRuntimeHandle, PluginRuntimeOptions,
};
pub use runtime::{PluginInstance, PluginRegistry, PluginRegistryError, ThemePluginInstance};
pub use toml::{
    EmulatorPlugin, PackageError, PluginCache, PluginPackage, PluginResource, PluginResourceFormat,
    ThemeDebug, ThemePlugin,
};

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::ModuleSpecifier;
    use snowglass_config::{
        ConfigurationToml,
        ast::{
            ConfigurationDefinition, ConfigurationInstance, DefaultDefinition, DomainDefinition,
            PortAssignments, RealizedDomain, RealizedOption, Value, ValueKind,
        },
    };
    use std::{collections::BTreeMap, convert::Infallible, sync::Arc};
    use toml_spanner::Arena;

    fn configuration() -> snowglass_config::ast::ConfigurationInstance {
        let source = r#"
            [targets.settings]
            output = { format = "ini", path = "settings.ini" }

            [sections.general]
            title = "General"
            target = "settings"

            [[sections.general.options]]
            key = "enabled"
            title = "Enabled"
            type = "bool"
            default = false
            emit = { key = "Enabled" }
        "#;
        let arena = Arena::new();
        let parsed = ConfigurationToml::try_from_tomls(&arena, [source].into_iter()).unwrap();
        let definition = Arc::new(ConfigurationDefinition::from(parsed));
        let realized = definition
            .resolve_with(|_, option| -> Result<_, Infallible> {
                let domain = match &option.value.kind {
                    ValueKind::Select {
                        domain: DomainDefinition::Static(_),
                    } => RealizedDomain::Static,
                    _ => RealizedDomain::None,
                };
                let default = match &option.value.default {
                    DefaultDefinition::Literal(value) => value.clone(),
                    DefaultDefinition::None | DefaultDefinition::Resolver(_) => Value::Null,
                };
                Ok(RealizedOption { default, domain })
            })
            .unwrap();
        realized.instantiate("example", BTreeMap::new()).unwrap()
    }

    struct TestPlugin {
        loader: PluginModuleLoader,
        entrypoint: ModuleSpecifier,
    }

    impl PluginInstance for TestPlugin {
        fn id(&self) -> &str {
            "test"
        }

        fn version(&self) -> &str {
            "0"
        }

        fn class(&self) -> &str {
            "emulator"
        }

        fn module_loader(&self) -> &PluginModuleLoader {
            &self.loader
        }
    }

    impl EmulatorPluginInstance for TestPlugin {
        fn entrypoint(&self) -> &ModuleSpecifier {
            &self.entrypoint
        }

        fn platforms(&self) -> &[String] {
            &[]
        }

        fn ports(&self, _console: &str) -> Result<Vec<EmulatorPort>, PackageError> {
            unreachable!()
        }

        fn port_assignments(
            &self,
            _console: &str,
            _layouts: &[String],
        ) -> Result<PortAssignments, PackageError> {
            unreachable!()
        }

        fn materialize(
            &self,
            _console: &str,
            _assignments: PortAssignments,
        ) -> Result<ConfigurationInstance, PackageError> {
            unreachable!()
        }
    }

    #[test]
    fn registry_projects_one_plugin_through_a_generic_interface_lookup() {
        let plugins = PluginRegistry::new();
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("plugins")
            .join("cemu")
            .join("plugin.toml");
        plugins.load(&manifest).unwrap();

        let base = plugins.plugin::<dyn PluginInstance>("cemu").unwrap();
        let emulator = plugins
            .plugin::<dyn EmulatorPluginInstance>("cemu")
            .unwrap();
        assert_eq!(base.id(), "cemu");
        assert_eq!(emulator.id(), "cemu");
        assert_eq!(
            base.caches(),
            &[PluginCache {
                key: "shaderCache".to_owned(),
                label: "Shader Cache".to_owned(),
            }]
        );
        assert_eq!(
            serde_json::to_value(plugins.descriptors()).unwrap(),
            serde_json::json!([{
                "id": "cemu",
                "version": "0.0.1",
                "class": "emulator",
                "caches": [{
                    "key": "shaderCache",
                    "label": "Shader Cache",
                }],
            }])
        );
        assert!(plugins.plugin::<TestPlugin>("cemu").is_none());
        assert_eq!(
            plugins
                .plugins::<dyn EmulatorPluginInstance>()
                .into_iter()
                .map(|plugin| plugin.id().to_owned())
                .collect::<Vec<_>>(),
            ["cemu"]
        );
        assert!(matches!(
            plugins.load(manifest),
            Err(PluginRegistryError::DuplicatePlugin(id)) if id == "cemu"
        ));
    }

    #[test]
    fn configurations_are_scoped_by_invocation_id() {
        let mut configurations = configuration::ConfigurationInstances::default();
        let first = configurations.insert(configuration());
        let second = configurations.insert(configuration());
        assert_ne!(first, second);

        configurations
            .set(first, "general.enabled", serde_json::json!(true))
            .unwrap();
        assert_eq!(
            configurations.get(first, "general.enabled").unwrap(),
            Some(serde_json::json!(true))
        );
        assert_eq!(
            configurations.get(second, "general.enabled").unwrap(),
            Some(serde_json::json!(false))
        );
        assert!(
            configurations
                .entries(first)
                .unwrap()
                .contains(&("general.enabled".to_owned(), serde_json::json!(true)))
        );
        assert!(
            configurations
                .entries(second)
                .unwrap()
                .contains(&("general.enabled".to_owned(), serde_json::json!(false)))
        );

        configurations.take(first).unwrap();
        assert!(matches!(
            configurations.get(first, "general.enabled"),
            Err(ConfigurationAccessError::UnknownConfiguration(id)) if id == first
        ));
        assert!(configurations.get(second, "general.enabled").is_ok());
    }
}
