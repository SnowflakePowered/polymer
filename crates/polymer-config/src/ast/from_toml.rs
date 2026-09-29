use std::{collections::BTreeMap, sync::Arc};

use crate::{
    ConfigurationToml,
    arena::Handle,
    toml::{
        config::{
            Emit, OptionDefinition as TomlOptionDefinition, OptionValue, SelectDefinition,
            Section, Target,
        },
        input::{InputBlock, LayoutMapping, Port, PortTable},
    },
};

use super::*;

type OwnedLayoutMapping = (
    indexmap::IndexMap<consolespec::input::BindingKey, Box<str>>,
    Box<[Handle<OptionDefinition>]>,
);

impl<'de> From<ConfigurationToml<'de>> for ConfigurationDefinition {
    fn from(configuration: ConfigurationToml<'de>) -> Self {
        let mut definition = ConfigurationDefinition::new();
        definition.meta = configuration
            .meta
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        definition.bool_styles = configuration
            .bool_styles
            .into_iter()
            .map(|(key, style)| {
                (
                    key.into(),
                    BoolStyleDefinition {
                        r#true: style.r#true.into(),
                        r#false: style.r#false.into(),
                    },
                )
            })
            .collect();

        for (key, target) in configuration.targets {
            definition.lower_target(None, key, target);
        }

        definition.root_sections = configuration
            .sections
            .into_iter()
            .map(|(key, section)| definition.lower_section(None, key, section))
            .collect();

        definition.actions = configuration.actions.map(|actions| ActionsDefinition {
            target: actions.target.to_owned_path(),
            emit: actions.emit.into_iter().map(EmitDefinition::from).collect(),
            map: actions
                .map
                .into_iter()
                .map(|(key, action)| {
                    (
                        key.into(),
                        ActionDefinition {
                            name: action.name.into(),
                            expression: action.expression.into(),
                        },
                    )
                })
                .collect(),
        });

        definition.input = configuration
            .input
            .map(|input| definition.lower_input(input, configuration.platforms));
        let _ = definition.target_graph();
        definition
    }
}

impl ConfigurationDefinition {
    fn lower_target<'de>(
        &mut self,
        parent: Option<Handle<TargetDefinition>>,
        key: &'de str,
        target: Target<'de>,
    ) -> Handle<TargetDefinition> {
        let path = parent.map_or_else(
            || OwnedTargetPath::from_key(Arc::from(key)),
            |parent| self.targets[parent].path.join::<family::Target>(key),
        );
        let output = target.output.map(|output| TargetOutputDefinition {
            format: output.format.into(),
            path: output.path.into(),
        });
        let additional_parents = target
            .parent
            .into_iter()
            .map(|path| path.to_owned_path())
            .collect();
        let children = target.children;

        let handle = self.targets.append(TargetDefinition {
            path,
            output,
            repeat: target.repeat,
            parent,
            additional_parents,
        });
        for (key, target) in children {
            self.lower_target(Some(handle), key, target);
        }
        handle
    }

    fn lower_section<'de>(
        &mut self,
        parent: Option<(Handle<SectionDefinition>, Option<OwnedTargetPath>)>,
        key: &'de str,
        section: Section<'de>,
    ) -> Handle<SectionDefinition> {
        let path = parent.as_ref().map_or_else(
            || SectionPath::from_key(Arc::from(key)),
            |(parent, _)| self.sections[*parent].path.join::<family::Section>(key),
        );
        let inherited_target = parent.and_then(|(_, target)| target);
        let target = section
            .target
            .map(|path| path.to_owned_path())
            .or(inherited_target);
        let options = section
            .options
            .into_iter()
            .map(|option| self.lower_option(option))
            .collect();
        let sections = section.sections;

        let handle = self.sections.append(SectionDefinition {
            path,
            title: section.title.into(),
            target: target.clone(),
            repeat: section.repeat,
            children: Box::new([]),
            options,
        });
        let children = sections
            .into_iter()
            .map(|(key, section)| self.lower_section(Some((handle, target.clone())), key, section))
            .collect();
        self.sections[handle].children = children;
        handle
    }

    fn lower_option<'de>(&mut self, option: TomlOptionDefinition<'de>) -> Handle<OptionDefinition> {
        let kind = match option.kind {
            "bool" => ValueKind::Boolean,
            "int" => ValueKind::Integer {
                range: option.range.map(|range| OptionRange::Integer {
                    min: range.min,
                    max: range.max,
                }),
            },
            "float" => ValueKind::Float {
                range: option.range.map(|range| OptionRange::Float {
                    min: range.min as f64,
                    max: range.max as f64,
                }),
            },
            "string" => ValueKind::String,
            "path" => ValueKind::Path,
            "select" => ValueKind::Select {
                domain: option.domain.into(),
            },
            kind => panic!("unsupported configuration option type `{kind}`"),
        };
        let default = DefaultDefinition::lower(option.default, &kind);
        let value = ValueDefinition { kind, default };

        self.options.append(OptionDefinition {
            key: option.key.into(),
            title: option.title.into(),
            value,
            emit: option.emit.into(),
        })
    }

    fn lower_input<'de>(
        &mut self,
        input: InputBlock<'de>,
        platforms: BTreeMap<&'de str, PortTable<'de>>,
    ) -> InputDefinition {
        let (mapping, options) = self.lower_layout_mapping(input.mapping);
        let mouse = input
            .mouse
            .map(|table| {
                table
                    .mapping
                    .into_iter()
                    .map(|(key, value)| (key, value.into()))
                    .collect()
            })
            .unwrap_or_default();
        InputDefinition {
            driver: input.driver.into(),
            device: input.device.into(),
            mapping,
            mouse,
            options,
            layouts: input
                .layouts
                .into_iter()
                .map(|(key, layout)| {
                    let (mapping, options) = self.lower_layout_mapping(layout);
                    (key.into(), LayoutDefinition { mapping, options })
                })
                .collect(),
            platforms: platforms
                .into_iter()
                .map(|(key, platform)| (key.into(), platform.lower()))
                .collect(),
        }
    }

    fn lower_layout_mapping(&mut self, mapping: LayoutMapping<'_>) -> OwnedLayoutMapping {
        (
            mapping
                .mapping
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            mapping
                .options
                .into_iter()
                .map(|option| self.lower_option(option))
                .collect(),
        )
    }
}

impl DefaultDefinition {
    fn lower(default: Option<OptionValue<'_>>, kind: &ValueKind) -> Self {
        default.map_or(Self::None, |value| {
            let value = match (kind, value) {
                (ValueKind::Select { .. }, OptionValue::String(key)) => Value::Choice(key.into()),
                (ValueKind::Path, OptionValue::String(path)) => Value::Path(path.into()),
                (_, value) => value.into(),
            };
            Self::Literal(value)
        })
    }
}

impl From<Option<Vec<SelectDefinition<'_>>>> for DomainDefinition {
    fn from(domain: Option<Vec<SelectDefinition<'_>>>) -> Self {
        match domain {
            None => Self::None,
            Some(choices) => Self::Static(
                choices
                    .into_iter()
                    .map(|choice| Choice {
                        key: ChoiceKey::new(choice.key),
                        label: choice.label.into(),
                        value: choice.value.into(),
                    })
                    .collect(),
            ),
        }
    }
}

impl From<OptionValue<'_>> for Value {
    fn from(value: OptionValue<'_>) -> Self {
        match value {
            OptionValue::Boolean(value) => Self::Boolean(value),
            OptionValue::Integer(value) => Self::Integer(value),
            OptionValue::Float(value) => Self::Float(value),
            OptionValue::String(value) => Self::String(value.into()),
        }
    }
}

impl From<Emit<'_>> for EmitDefinition {
    fn from(emit: Emit<'_>) -> Self {
        Self {
            target: emit.target.map(|path| path.to_owned_path()),
            key: emit.key.into(),
            value: emit.value.map(Value::from),
            bool_style: emit.bool_style.map(Into::into),
            int_style: emit.int_style.map(Into::into),
            xml_attribute: emit.xml.is_some_and(|xml| xml.attribute),
        }
    }
}

impl PortTable<'_> {
    fn lower(self) -> PlatformDefinition {
        self.ports
            .into_iter()
            .map(|(key, ports)| {
                let mut ports: Vec<_> = ports
                    .into_iter()
                    .map(|(index, port)| {
                        let index = index
                            .parse()
                            .unwrap_or_else(|_| panic!("port index `{index}` is not an integer"));
                        PortDefinition::lower(index, port)
                    })
                    .collect();
                ports.sort_by_key(|port| port.index);
                (key.into(), ports.into_boxed_slice())
            })
            .collect()
    }
}

impl PortDefinition {
    fn lower(index: usize, port: Port<'_>) -> Self {
        Self {
            index,
            target: port.target.to_owned_path(),
            layouts: port.layouts.into_iter().map(Into::into).collect(),
            emit: port
                .emit
                .into_iter()
                .map(|(choice, emits)| {
                    (
                        choice.into(),
                        emits.into_iter().map(EmitDefinition::from).collect(),
                    )
                })
                .collect(),
        }
    }
}
