use std::{collections::BTreeMap, sync::Arc};

use crate::{
    ConfigurationToml,
    arena::Handle,
    toml::{
        config::{
            Emit, OptionDefinition as TomlOptionDefinition, OptionDomain, OptionValue, Section,
            Target,
        },
        input::{InputBlock, LayoutMapping, PortAssignment, PortTable},
    },
};

use super::*;

type OwnedLayoutMapping = (
    indexmap::IndexMap<consolespec::BindingKey, Box<str>>,
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

        definition.root_targets = configuration
            .targets
            .into_iter()
            .map(|(key, target)| lower_target(&mut definition, None, key, target))
            .collect();

        definition.root_sections = configuration
            .sections
            .into_iter()
            .map(|(key, section)| lower_section(&mut definition, None, key, section))
            .collect();

        definition.input = configuration
            .input
            .map(|input| lower_input(&mut definition, input, configuration.platforms));
        definition
    }
}

fn lower_target<'de>(
    definition: &mut ConfigurationDefinition,
    parent: Option<Handle<TargetDefinition>>,
    key: &'de str,
    target: Target<'de>,
) -> Handle<TargetDefinition> {
    let path = parent.map_or_else(
        || OwnedTargetPath::from_key(Arc::from(key)),
        |parent| definition.targets[parent].path.join::<family::Target>(key),
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

    let handle = definition.targets.append(TargetDefinition {
        path,
        output,
        parent,
        additional_parents,
        children: Box::new([]),
    });
    let children = children
        .into_iter()
        .map(|(key, target)| lower_target(definition, Some(handle), key, target))
        .collect();
    definition.targets[handle].children = children;
    handle
}

fn lower_section<'de>(
    definition: &mut ConfigurationDefinition,
    parent: Option<Handle<SectionDefinition>>,
    key: &'de str,
    section: Section<'de>,
) -> Handle<SectionDefinition> {
    let path = parent.map_or_else(
        || SectionPath::from_key(Arc::from(key)),
        |parent| {
            definition.sections[parent]
                .path
                .join::<family::Section>(key)
        },
    );
    let options = section
        .options
        .into_iter()
        .map(|option| lower_option(definition, option))
        .collect();
    let sections = section.sections;

    let handle = definition.sections.append(SectionDefinition {
        key: key.into(),
        path,
        title: section.title.into(),
        description: None,
        target: section.target.map(|path| path.to_owned_path()),
        parent,
        children: Box::new([]),
        options,
    });
    let children = sections
        .into_iter()
        .map(|(key, section)| lower_section(definition, Some(handle), key, section))
        .collect();
    definition.sections[handle].children = children;
    handle
}

fn lower_option<'de>(
    definition: &mut ConfigurationDefinition,
    option: TomlOptionDefinition<'de>,
) -> Handle<OptionDefinition> {
    let value = match option.kind {
        "bool" => ValueDefinition::Boolean {
            default: lower_default(option.default, false),
        },
        "int" => ValueDefinition::Integer {
            range: option.range.map(|range| OptionRange::Integer {
                min: range.min,
                max: range.max,
            }),
            default: lower_default(option.default, false),
        },
        "float" => ValueDefinition::Float {
            range: option.range.map(|range| OptionRange::Float {
                min: range.min as f64,
                max: range.max as f64,
            }),
            default: lower_default(option.default, false),
        },
        "string" => ValueDefinition::String {
            default: lower_default(option.default, false),
        },
        "select" => ValueDefinition::Select {
            domain: lower_domain(option.domain),
            default: lower_default(option.default, true),
        },
        kind => panic!("unsupported configuration option type `{kind}`"),
    };

    definition.options.append(OptionDefinition {
        key: option.key.into(),
        title: option.title.into(),
        description: None,
        value,
        emit: lower_emit(option.emit),
    })
}

fn lower_default(default: Option<OptionValue<'_>>, choice: bool) -> DefaultDefinition {
    default.map_or(DefaultDefinition::None, |value| {
        let value = if choice {
            match value {
                OptionValue::String(key) => Value::Choice(key.into()),
                other => lower_value(other),
            }
        } else {
            lower_value(value)
        };
        DefaultDefinition::Literal(value)
    })
}

fn lower_domain(domain: Option<OptionDomain<'_>>) -> DomainDefinition {
    match domain {
        None => DomainDefinition::None,
        Some(OptionDomain::Resolve { source }) => {
            DomainDefinition::Resolver(ResolverSource::new(source))
        }
        Some(OptionDomain::Select(choices)) => DomainDefinition::Static(
            choices
                .into_iter()
                .map(|choice| Choice {
                    key: ChoiceKey::new(choice.key),
                    label: choice.label.into(),
                    value: lower_value(choice.value),
                })
                .collect(),
        ),
    }
}

fn lower_value(value: OptionValue<'_>) -> Value {
    match value {
        OptionValue::Boolean(value) => Value::Boolean(value),
        OptionValue::Integer(value) => Value::Integer(value),
        OptionValue::Float(value) => Value::Float(value),
        OptionValue::String(value) => Value::String(value.into()),
    }
}

fn lower_emit(emit: Emit<'_>) -> EmitDefinition {
    EmitDefinition {
        target: emit.target.map(|path| path.to_owned_path()),
        key: emit.key.into(),
        value: emit.value.map(lower_value),
        bool_style: emit.bool_style.map(Into::into),
        int_style: emit.int_style.map(Into::into),
    }
}

fn lower_input<'de>(
    definition: &mut ConfigurationDefinition,
    input: InputBlock<'de>,
    platforms: BTreeMap<&'de str, PortTable<'de>>,
) -> InputDefinition {
    let (mapping, options) = lower_layout_mapping(definition, input.mapping);
    InputDefinition {
        driver: input.driver.into(),
        device: input.device.into(),
        mapping,
        options,
        layouts: input
            .layouts
            .into_iter()
            .map(|(key, layout)| {
                let (mapping, options) = lower_layout_mapping(definition, layout);
                (key.into(), LayoutDefinition { mapping, options })
            })
            .collect(),
        platforms: platforms
            .into_iter()
            .map(|(key, platform)| (key.into(), lower_platform(platform)))
            .collect(),
    }
}

fn lower_layout_mapping(
    definition: &mut ConfigurationDefinition,
    mapping: LayoutMapping<'_>,
) -> OwnedLayoutMapping {
    (
        mapping
            .mapping
            .into_iter()
            .map(|(key, value)| (key, value.into()))
            .collect(),
        mapping
            .options
            .into_iter()
            .map(|option| lower_option(definition, option))
            .collect(),
    )
}

fn lower_platform(platform: PortTable<'_>) -> PlatformDefinition {
    PlatformDefinition {
        groups: platform
            .ports
            .into_iter()
            .map(|(key, ports)| {
                let mut ports: Vec<_> = ports
                    .into_iter()
                    .map(|(index, port)| {
                        let index = index
                            .parse()
                            .unwrap_or_else(|_| panic!("port index `{index}` is not an integer"));
                        lower_port(index, port)
                    })
                    .collect();
                ports.sort_by_key(|port| port.index);
                (
                    key.into(),
                    PortGroupDefinition {
                        ports: ports.into_boxed_slice(),
                    },
                )
            })
            .collect(),
    }
}

fn lower_port(index: usize, port: PortAssignment<'_>) -> PortDefinition {
    PortDefinition {
        index,
        target: port.target.to_owned_path(),
        layouts: port.layouts.into_iter().map(Into::into).collect(),
        emit: port
            .emit
            .into_iter()
            .map(|(choice, emits)| (choice.into(), emits.into_iter().map(lower_emit).collect()))
            .collect(),
    }
}
