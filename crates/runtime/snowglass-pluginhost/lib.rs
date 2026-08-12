//! Emulator-plugin configuration host.
//!
//! The embedding application owns its JavaScript runtime. This crate owns the
//! adapter implementation that temporarily borrows that runtime, plus the
//! invocation-scoped configuration exposed to a plugin. Pluginhost owns the
//! emulator-facing output contract but leaves its interpretation to the viewer.

#![forbid(unsafe_code)]

mod configuration;
pub mod deno;
mod emulator;
mod module_loader;
mod runtime;
pub mod toml;

pub use configuration::{ConfigurationAccessError, SerializedConfiguration};
pub use deno::EmulatorLaunch;
pub use emulator::{
    EmulatorHostError, EmulatorLaunchArguments, EmulatorPluginInstance, EmulatorPort,
    EmulatorSetupInvocation, GraphicsHook, TrackedSurface,
};
pub use module_loader::{PluginModuleLoader, PluginModuleRouter};
pub use runtime::{PluginHost, PluginInstance, PluginRegistryError};
pub use toml::{EmulatorPlugin, PackageError, PluginPackage};

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{
        JsRuntime, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader,
        ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
        RuntimeOptions, error::ModuleLoaderError, resolve_import,
    };
    use snowglass_config::{
        ConfigurationToml,
        ast::{
            ConfigurationDefinition, ConfigurationInstance, DefaultDefinition, DomainDefinition,
            PortAssignments, RealizedDomain, RealizedOption, Value, ValueKind,
        },
    };
    use std::{collections::BTreeMap, convert::Infallible, rc::Rc, sync::Arc};
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

    impl TestPlugin {
        fn new() -> Self {
            Self {
                loader: PluginModuleLoader::new("test", std::env::current_dir().unwrap()),
                entrypoint: ModuleSpecifier::parse("file:///plugin.js").unwrap(),
            }
        }
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

    #[derive(Default)]
    struct MemoryLoader;

    impl ModuleLoader for MemoryLoader {
        fn resolve(
            &self,
            specifier: &str,
            referrer: &str,
            _kind: ResolutionKind,
        ) -> Result<ModuleSpecifier, ModuleLoaderError> {
            resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)
        }

        fn load(
            &self,
            module_specifier: &ModuleSpecifier,
            _maybe_referrer: Option<&ModuleLoadReferrer>,
            _options: ModuleLoadOptions,
        ) -> ModuleLoadResponse {
            let source = if module_specifier.as_str() == "file:///plugin.js" {
                r#"
                    export default {
                      setupLaunch(args) {
                        const entries = args.emulatorConfig.entries();
                        if (!Array.isArray(entries) ||
                            entries.find(([path]) => path === "general.enabled")?.[1] !== false) {
                          throw new Error("configuration entries did not include the default");
                        }
                        if (args.emulatorConfig.options().general.enabled !== false) {
                          throw new Error("configuration read did not return the default");
                        }
                        let rejected = false;
                        try {
                          args.emulatorConfig.options().general.enabled = "yes";
                        } catch {
                          rejected = true;
                        }
                        if (!rejected) {
                          throw new Error("configuration accepted the wrong value type");
                        }
                        args.emulatorConfig.options().general.enabled = true;
                        return {
                          command: "emulator.exe",
                          argv: [],
                          configRoot: ".",
                          config: args.emulatorConfig,
                          isolate: false,
                          hooks: ["vulkan"],
                          surfaces: [{ index: 0, pattern: "window" }],
                        };
                      },
                    };
                "#
            } else {
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::type_error(format!(
                    "unknown test module {module_specifier}"
                ))));
            };
            ModuleLoadResponse::Sync(Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(source.to_owned().into()),
                module_specifier,
                None,
            )))
        }
    }

    #[test]
    fn registry_projects_one_plugin_through_a_generic_interface_lookup() {
        let plugins = PluginHost::new();
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

    #[tokio::test(flavor = "current_thread")]
    async fn concrete_emulator_launch_realizes_config_before_returning_viewer_args() {
        // The test, like the shell, creates and owns the runtime. Pluginhost
        // only supplies an extension and temporarily borrows it below.
        let mut deno = JsRuntime::new(RuntimeOptions {
            extensions: vec![deno::extension()],
            module_loader: Some(Rc::new(MemoryLoader)),
            ..Default::default()
        });
        let mut launch = EmulatorLaunch::attach(&mut deno);
        let args = launch
            .execute(EmulatorSetupInvocation {
                plugin: Arc::new(TestPlugin::new()),
                arguments: serde_json::json!({ "launchPath": "game" }),
                configuration: configuration(),
            })
            .await
            .unwrap();

        assert_eq!(args.command, "emulator.exe");
        let settings = std::str::from_utf8(&args.config["settings.ini"]).unwrap();
        assert!(settings.contains("Enabled=true"), "{settings}");
        assert!(
            deno.execute_script(
                "snowglass:pluginhost/inactive-configuration.js",
                "__snowglassPluginHost.configuration(1).options().general.enabled",
            )
            .is_err(),
            "configuration remained active after host serialization"
        );
    }
}
