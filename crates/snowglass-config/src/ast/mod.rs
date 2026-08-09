mod definition;
mod from_toml;
mod instance;
mod path;
mod realized;
mod target;
mod value;

pub use definition::*;
pub use instance::*;
pub use path::*;
pub use realized::*;
pub use target::*;
pub use value::*;

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use indexmap::IndexMap;

    use super::*;
    use crate::Handle;

    fn fixture() -> (Arc<RealizedDefinition>, Handle<OptionDefinition>) {
        let mut definition = ConfigurationDefinition::new();
        let backend = definition.options.append(OptionDefinition {
            key: "backend".into(),
            title: "Backend".into(),
            value: ValueDefinition {
                kind: ValueKind::Select {
                    domain: DomainDefinition::Static(
                        vec![Choice {
                            key: ChoiceKey::from("vulkan"),
                            label: "Vulkan".into(),
                            value: Value::String("Vulkan".into()),
                        }]
                        .into_boxed_slice(),
                    ),
                },
                default: DefaultDefinition::Literal(Value::Choice("vulkan".into())),
            },
            emit: emit("Backend"),
        });
        let sideways = definition.options.append(OptionDefinition {
            key: "sideways".into(),
            title: "Sideways Wii Remote".into(),
            value: ValueDefinition {
                kind: ValueKind::Boolean,
                default: DefaultDefinition::Literal(Value::Boolean(false)),
            },
            emit: emit("Sideways"),
        });

        let graphics = definition.sections.append(SectionDefinition {
            path: SectionPath::new("graphics").unwrap(),
            title: "Graphics".into(),
            target: None,
            children: Box::new([]),
            options: Box::new([backend]),
        });
        definition.root_sections = Box::new([graphics]);

        definition.input = Some(InputDefinition {
            driver: "SDL".into(),
            device: "SDL/{index}".into(),
            mapping: IndexMap::new(),
            options: Box::new([]),
            layouts: BTreeMap::from([(
                "WII_REMOTE_CONTROLLER".into(),
                LayoutDefinition {
                    mapping: IndexMap::new(),
                    options: Box::new([sideways]),
                },
            )]),
            platforms: BTreeMap::from([(
                "NINTENDO_WII".into(),
                BTreeMap::from([(
                    "wii".into(),
                    Box::new([PortDefinition {
                        index: 0,
                        target: OwnedTargetPath::new("wiipad.Wiimote1").unwrap(),
                        layouts: Box::new(["WII_REMOTE_CONTROLLER".into()]),
                        emit: BTreeMap::new(),
                    }]) as PortGroupDefinition,
                )]),
            )]),
        });

        let definition = Arc::new(definition);
        let realized = definition
            .resolve_with(|_handle, option| -> Result<_, std::convert::Infallible> {
                let domain = match &option.value.kind {
                    ValueKind::Select { domain } => match domain {
                        DomainDefinition::Static(_) => RealizedDomain::Static,
                        DomainDefinition::None | DomainDefinition::Resolver(_) => {
                            RealizedDomain::None
                        }
                    },
                    _ => RealizedDomain::None,
                };
                Ok(RealizedOption {
                    default: literal(&option.value.default),
                    domain,
                })
            })
            .unwrap();

        (realized, backend)
    }

    fn emit(key: &str) -> EmitDefinition {
        EmitDefinition {
            target: None,
            key: key.into(),
            value: None,
            bool_style: None,
            int_style: None,
        }
    }

    fn literal(default: &DefaultDefinition) -> Value {
        match default {
            DefaultDefinition::Literal(value) => value.clone(),
            DefaultDefinition::None | DefaultDefinition::Resolver(_) => Value::Null,
        }
    }

    #[test]
    fn realized_definition_reuses_static_structure_for_the_frontend() {
        let (definition, backend) = fixture();
        let option = definition.option(backend);

        assert_eq!(option.definition.title.as_ref(), "Backend");
        assert_eq!(option.realized.default, Value::Choice("vulkan".into()));
        assert_eq!(
            definition.choices(backend).unwrap()[0].label.as_ref(),
            "Vulkan"
        );
        assert_eq!(definition.root_sections().count(), 1);
    }

    #[test]
    fn instance_flattens_section_and_selected_layout_values() {
        let (definition, _) = fixture();
        let assignments = BTreeMap::from([(
            "wii".into(),
            vec![Box::<str>::from("WII_REMOTE_CONTROLLER")].into_boxed_slice(),
        )]);
        let mut instance = definition.instantiate("NINTENDO_WII", assignments).unwrap();

        assert_eq!(instance.len(), 2);
        assert_eq!(instance["graphics.backend"], Value::Choice("vulkan".into()));
        assert_eq!(
            instance["ports.NINTENDO_WII.wii.0.sideways"],
            Value::Boolean(false)
        );

        instance["graphics.backend"] = Value::Choice("software".into());
        assert_eq!(
            instance.get("graphics.backend"),
            Some(&Value::Choice("software".into()))
        );
        assert_eq!((&instance).into_iter().count(), 2);
    }

    #[test]
    fn instance_rejects_a_layout_the_port_does_not_accept() {
        let (definition, _) = fixture();
        let assignments = BTreeMap::from([(
            "wii".into(),
            vec![Box::<str>::from("GAMECUBE_CONTROLLER")].into_boxed_slice(),
        )]);
        let error = definition
            .instantiate("NINTENDO_WII", assignments)
            .err()
            .expect("assignment to fail");

        assert!(matches!(
            error,
            crate::error::Error::LayoutNotAccepted { index: 0, .. }
        ));
    }

    #[test]
    fn converts_the_borrowed_toml_tree_to_an_owned_flat_definition() {
        let source = String::from(
            r#"
                [meta]
                name = "Example"

                [bool_style.upper]
                true = "True"
                false = "False"

                [targets.main]
                output = { format = "ini", path = "main.ini" }

                [targets.main.Core]

                [sections.general]
                title = "General"
                target = "main.Core"

                [[sections.general.options]]
                key = "enabled"
                title = "Enabled"
                type = "bool"
                default = true
                emit = { key = "Enabled", bool_style = "upper" }

                [input]
                driver = "SDL"
                device = "SDL/{index}"

                [input.mapping]
                [input.layouts.GAMECUBE_CONTROLLER]

                [platforms.NINTENDO_GCN.ports.controller.0]
                target = "main.Core"
                layouts = ["GAMECUBE_CONTROLLER"]
                emit.unplugged = [{ key = "SIDevice0", value = "0" }]
            "#,
        );

        let definition = {
            let arena = toml_spanner::Arena::new();
            let parsed =
                crate::ConfigurationToml::try_from_tomls(&arena, std::iter::once(source.as_str()))
                    .unwrap();
            ConfigurationDefinition::from(parsed)
        };

        assert_eq!(definition.meta["name"].as_ref(), "Example");
        assert_eq!(definition.targets.len(), 2);
        assert_eq!(definition.sections.len(), 1);
        assert_eq!(definition.options.len(), 1);
        assert_eq!(definition.target_graph().unwrap().output_roots().len(), 1);
        assert_eq!(
            definition.sections[definition.root_sections[0]]
                .path
                .as_str(),
            "general"
        );
        assert_eq!(
            definition.input.as_ref().unwrap().platforms["NINTENDO_GCN"]["controller"][0].index,
            0
        );
    }

    #[test]
    fn lowering_compiles_and_validates_the_target_graph() {
        let source = r#"
            [targets.first]
            parent = ["second"]

            [targets.second]
            parent = ["first"]
        "#;
        let arena = toml_spanner::Arena::new();
        let parsed =
            crate::ConfigurationToml::try_from_tomls(&arena, std::iter::once(source)).unwrap();
        let definition = ConfigurationDefinition::from(parsed);

        assert!(matches!(
            definition.target_graph(),
            Err(crate::error::Error::TargetCycle(_))
        ));
    }
}
