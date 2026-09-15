//! Isolated Polymer plugin registry and runtime.
//!
//! Package discovery is kept separate from execution. Every call made through
//! [`runtime::PluginRuntimeHandle`] runs in a fresh curated Deno/V8 isolate owned by this
//! crate, and invocation-scoped resources are destroyed with that isolate.

#![forbid(unsafe_code)]

pub mod actions;
mod automation;
mod cache;
mod configuration;
pub mod controller;
mod database;
pub mod digest;
pub mod emulator;
pub mod filesystem;
mod handle;
pub mod loading;
pub mod platform;
pub mod plugin;
mod plugin_storage;
pub mod runtime;
pub mod scraper;
pub mod theme;

#[cfg(test)]
mod tests {
    use crate::configuration::{ConfigurationAccessError, ConfigurationInstances};
    use polymer_config::{ConfigurationToml, ast::ConfigurationDefinition};
    use std::{collections::BTreeMap, sync::Arc};
    use toml_spanner::Arena;

    fn configuration() -> polymer_config::ast::ConfigurationInstance {
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

    #[test]
    fn configurations_are_scoped_by_invocation_id() {
        let mut configurations = ConfigurationInstances::default();
        let topology = polymer_controllers::ControllerTopology::default();
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
