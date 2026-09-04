//! Isolated Snowglass plugin registry and runtime.
//!
//! Package discovery is kept separate from execution. Every call made through
//! [`PluginRuntimeHandle`] runs in a fresh curated Deno/V8 isolate owned by this
//! crate, and invocation-scoped resources are destroyed with that isolate.

#![forbid(unsafe_code)]

mod actions;
mod automation;
mod cache;
mod configuration;
mod controller;
mod database;
mod deno;
pub mod descriptor;
mod emulator;
mod filesystem;
mod handle;
mod module_loader;
mod plugin_runtime;
mod runtime;
mod scraper;
pub mod toml;

pub use actions::ParsedHotkey;
pub use configuration::{ConfigurationAccessError, SerializedConfiguration};
pub use controller::{ControllerSession, PauseState};
pub(crate) use emulator::EmulatorLaunchInvocation;
pub use emulator::{
    EmulatorHostError, EmulatorLaunchArguments, EmulatorPluginInstance, EmulatorPort, GraphicsHook,
    Hooks, InputHook, QuickResumeData, TrackedSurface,
};
pub use filesystem::{StorageFactories, StorageFactory};
pub use module_loader::PluginModuleLoader;
pub use plugin_runtime::{
    PluginCancellationToken, PluginInvocationError, PluginInvocationResult, PluginRuntime,
    PluginRuntimeHandle, PluginRuntimeOptions,
};
pub use runtime::{PluginInstance, PluginRegistry, PluginRegistryError, ThemePluginInstance};
pub use scraper::{
    ScrapeCandidate, ScrapeInput, ScrapeResult, ScrapeScalar, ScraperPluginInstance,
};
pub use toml::{
    EmulatorPlugin, PackageError, PluginCache, PluginPackage, PluginResource, PluginResourceFormat,
    SaveStateConfig, ScrapeRequirement, ScraperPlugin, ThemeDebug, ThemePlugin,
};

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::ModuleSpecifier;
    use snowglass_config::{
        ConfigurationToml,
        ast::{ConfigurationDefinition, ConfigurationInstance, PortAssignments},
    };
    use std::{collections::BTreeMap, sync::Arc};
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
        definition.instantiate("example", BTreeMap::new()).unwrap()
    }

    #[allow(dead_code)]
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

        fn entrypoint(&self) -> &ModuleSpecifier {
            &self.entrypoint
        }
    }

    impl EmulatorPluginInstance for TestPlugin {
        fn configuration_definition(&self) -> &ConfigurationDefinition {
            unreachable!()
        }

        fn platforms(&self) -> &[String] {
            &[]
        }

        fn save_states(&self) -> Option<&SaveStateConfig> {
            None
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
    fn configurations_are_scoped_by_invocation_id() {
        let mut configurations = configuration::ConfigurationInstances::default();
        let topology = snowglass_controllers::ControllerTopology::default();
        let first = configurations.insert(configuration(), &topology);
        let second = configurations.insert(configuration(), &topology);
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
