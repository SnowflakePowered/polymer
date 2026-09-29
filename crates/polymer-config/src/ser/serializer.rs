use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use indexmap::IndexMap;

use crate::{
    Handle,
    ast::{
        BoolStyleDefinition, ConfigurationDefinition, ConfigurationInstance, EmitDefinition,
        InputDefinition, LayoutDefinition, OptionDefinition, OwnedTargetPath, SettingPath,
        TargetDefinition, TargetGraph, TargetOutputDefinition, Value,
    },
    error::{Error, Result},
};

use super::{
    BmlEmitter, DocumentRef, EmittedAttribute, EmittedBinding, EmittedValue, Emitter, IniEmitter,
    ScalarValue, SectionRef, XmlEmitter,
};
use crate::input::{
    gamepad::SdlGamepadIdentity,
    projection::{Projection, Target as ProjectionTarget},
};

/// The compiled serialization state for one configuration instance.
pub struct Serializer<'a> {
    instance: &'a ConfigurationInstance,
    definition: &'a ConfigurationDefinition,
    target_graph: &'a TargetGraph,
    writes: Vec<IndexMap<Option<Box<str>>, Vec<WritePlan<'a>>>>,
    bindings: Vec<Vec<BindingPlan>>,
    gamepad_bindings: BTreeMap<(Box<str>, usize), usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GamepadBinding {
    pub group: Box<str>,
    pub port: usize,
    pub gamepad: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Document<'a> {
    target: Handle<TargetDefinition>,
    path: &'a str,
    output: &'a TargetOutputDefinition,
}

impl<'a> Document<'a> {
    pub fn target(&self) -> Handle<TargetDefinition> {
        self.target
    }

    pub fn target_path(&self) -> &'a str {
        self.path
    }

    pub fn output(&self) -> &'a TargetOutputDefinition {
        self.output
    }
}

struct WritePlan<'a> {
    key: &'a str,
    source: WriteSource<'a>,
    bool_style: Option<&'a BoolStyleDefinition>,
    int_style: Option<&'a str>,
    attribute: bool,
}

enum WriteSource<'a> {
    Setting {
        path: Box<str>,
        value: &'a Value,
        option: Handle<OptionDefinition>,
    },
    Literal(&'a Value),
    OwnedString(Box<str>),
}

struct BindingPlan {
    key: Box<str>,
    value: Box<str>,
}

impl<'a> Serializer<'a> {
    pub fn new(
        instance: &'a ConfigurationInstance,
        bindings: impl IntoIterator<Item = GamepadBinding>,
    ) -> Result<Self> {
        let mut assigned = HashSet::new();
        let mut gamepad_bindings = BTreeMap::new();
        for binding in bindings {
            if !assigned.insert(binding.gamepad) {
                return Err(Error::DuplicateGamepadBinding {
                    index: binding.gamepad,
                });
            }
            let target = (binding.group, binding.port);
            if gamepad_bindings
                .insert(target.clone(), binding.gamepad)
                .is_some()
            {
                return Err(Error::DuplicatePortGamepadBinding {
                    group: target.0,
                    index: target.1,
                });
            }
        }
        let definition = instance.definition();
        let target_graph = definition.target_graph()?;

        let mut serializer = Self {
            instance,
            definition,
            target_graph,
            writes: (0..definition.targets.len())
                .map(|_| IndexMap::new())
                .collect(),
            bindings: (0..definition.targets.len()).map(|_| Vec::new()).collect(),
            gamepad_bindings,
        };
        serializer.validate_repetition()?;
        serializer.plan_section_writes()?;
        serializer.plan_input_writes()?;
        serializer.plan_port_writes()?;
        serializer.plan_action_writes()?;
        if let Some(((group, index), _)) = serializer.gamepad_bindings.first_key_value() {
            return Err(Error::UnexpectedGamepadBinding {
                group: group.clone(),
                index: *index,
            });
        }
        Ok(serializer)
    }

    pub fn instance(&self) -> &'a ConfigurationInstance {
        self.instance
    }

    pub fn definition(&self) -> &'a ConfigurationDefinition {
        self.definition
    }

    pub fn documents(&self) -> impl ExactSizeIterator<Item = Document<'a>> + '_ {
        self.target_graph
            .output_roots()
            .iter()
            .copied()
            .map(|handle| {
                let target = &self.definition.targets[handle];
                Document {
                    target: handle,
                    path: target.path.as_str(),
                    output: target
                        .output
                        .as_ref()
                        .expect("the target graph only retains output-bearing roots"),
                }
            })
    }

    pub fn serialize_document(
        &self,
        document: Document<'a>,
        mut emitter: Box<dyn Emitter>,
    ) -> Result<Vec<u8>> {
        let document_ref = DocumentRef {
            target: document.path,
            output: document.output,
        };
        emitter.begin_document(document_ref)?;
        self.write_values(document.target, None, emitter.as_mut())?;
        for child in self.target_graph.children(document.target) {
            if self.is_active(child, None) {
                self.write_section(child, None, emitter.as_mut())?;
            }
        }
        emitter.end_document(document_ref)?;
        let mut bytes = Vec::new();
        emitter.finish(&mut bytes)?;
        Ok(bytes)
    }

    /// Serializes every output-bearing root into a path-to-bytes map.
    pub fn serialize(&self) -> Result<BTreeMap<Box<str>, Vec<u8>>> {
        let mut outputs = BTreeMap::new();
        for document in self.documents() {
            if !self.is_active(document.target, None) {
                continue;
            }
            if outputs.contains_key(document.output.path.as_ref()) {
                return Err(Error::DuplicateOutputPath(document.output.path.clone()));
            }
            let emitter = Self::emitter(&document.output.format)
                .ok_or_else(|| Error::UnsupportedFormat(document.output.format.clone()))?;
            let bytes = self.serialize_document(document, emitter)?;
            outputs.insert(document.output.path.clone(), bytes);
        }
        Ok(outputs)
    }

    fn emitter(format: &str) -> Option<Box<dyn Emitter>> {
        match format {
            "bml" => Some(Box::new(BmlEmitter::new())),
            "ini" => Some(Box::new(IniEmitter::new())),
            "xml" => Some(Box::new(XmlEmitter::new())),
            _ => None,
        }
    }

    fn plan_section_writes(&mut self) -> Result<()> {
        for section in self.definition.root_sections() {
            self.plan_section(section, None, None)?;
        }
        Ok(())
    }

    fn plan_section(
        &mut self,
        handle: Handle<crate::ast::SectionDefinition>,
        concrete_parent: Option<&str>,
        instance_prefix: Option<&str>,
    ) -> Result<()> {
        let section = &self.definition.sections[handle];
        let local = section
            .path
            .components()
            .last()
            .unwrap_or(section.path.as_str());
        let concrete = concrete_parent.map_or_else(
            || section.path.as_str().to_owned(),
            |parent| format!("{parent}.{local}"),
        );
        if section.repeat {
            let count = self
                .instance
                .repeated_instances()
                .get(concrete.as_str())
                .copied()
                .unwrap_or(0);
            for index in 0..count {
                let prefix = format!("{concrete}.{index}");
                self.plan_section_options(handle, &prefix, Some(prefix.as_str()))?;
                for child in section.children.iter().copied() {
                    self.plan_section(child, Some(&prefix), Some(&prefix))?;
                }
            }
        } else {
            self.plan_section_options(handle, &concrete, instance_prefix)?;
            for child in section.children.iter().copied() {
                self.plan_section(child, Some(&concrete), instance_prefix)?;
            }
        }
        Ok(())
    }

    fn plan_section_options(
        &mut self,
        handle: Handle<crate::ast::SectionDefinition>,
        concrete: &str,
        instance_prefix: Option<&str>,
    ) -> Result<()> {
        let section = &self.definition.sections[handle];
        if section.repeat
            && let Some(target) = section.target.as_ref()
        {
            let target = self
                .target_graph
                .resolve(target.as_str())
                .ok_or_else(|| Error::UnknownTarget(target.as_str().into()))?;
            self.writes[target.index()]
                .entry(instance_prefix.map(Into::into))
                .or_default();
        }
        for option_handle in section.options.iter().copied() {
            let option = &self.definition.options[option_handle];
            let setting_path =
                SettingPath::from_key(Arc::from(format!("{concrete}.{}", option.key)));
            let Some(value) = self.instance.get(setting_path.as_str()) else {
                continue;
            };
            let source = option.emit.value.as_ref().map_or(
                WriteSource::Setting {
                    path: setting_path.as_str().into(),
                    value,
                    option: option_handle,
                },
                WriteSource::Literal,
            );
            self.add_emit(
                section.target.as_ref(),
                &option.emit,
                instance_prefix.map(Into::into),
                source,
            )?;
        }
        Ok(())
    }

    fn plan_input_writes(&mut self) -> Result<()> {
        let Some(input) = self.definition.input.as_ref() else {
            return Ok(());
        };
        for option_handle in input.options.iter().copied() {
            let option = &self.definition.options[option_handle];
            let setting_path = SettingPath::from_key(Arc::from(format!("input.{}", option.key)));
            let Some(value) = self.instance.get(setting_path.as_str()) else {
                continue;
            };
            let source = option.emit.value.as_ref().map_or(
                WriteSource::Setting {
                    path: setting_path.as_str().into(),
                    value,
                    option: option_handle,
                },
                WriteSource::Literal,
            );
            self.add_emit(None, &option.emit, None, source)?;
        }
        Ok(())
    }

    fn plan_port_writes(&mut self) -> Result<()> {
        let Some(input) = self.definition.input.as_ref() else {
            return Ok(());
        };
        let Some(platform) = input.platforms.get(self.instance.console()) else {
            return Ok(());
        };

        for (group_name, group) in platform {
            let assignments = self.instance.port_assignments().get(group_name.as_ref());
            for port in group {
                let assignment = assignments.and_then(|assignments| assignments.get(port.index));
                let layout_name = assignment.map_or("unplugged", |assignment| &*assignment.layout);
                let mouse = assignment.is_some_and(|assignment| assignment.mouse);

                let layout = input.layouts.get(layout_name);
                let device = layout
                    .map(|_| {
                        let index = self
                            .gamepad_bindings
                            .remove(&(group_name.clone(), port.index))
                            .ok_or_else(|| Error::MissingGamepadBinding {
                                group: group_name.clone(),
                                index: port.index,
                            })?;
                        expand_device(&input.device, index)
                    })
                    .transpose()?;

                if let Some(emits) = port.emit.get(layout_name) {
                    for emit in emits {
                        let Some(value) = emit.value.as_ref() else {
                            continue;
                        };
                        let source = match (value, device.as_deref()) {
                            (Value::String(value), Some(device)) => WriteSource::OwnedString(
                                expand_template(value, device)?.into_boxed_str(),
                            ),
                            _ => WriteSource::Literal(value),
                        };
                        self.add_emit(Some(&port.target), emit, None, source)?;
                    }
                }

                let Some(layout) = layout else {
                    continue;
                };
                let device = device.expect("a known layout has a virtual device");
                self.plan_layout_bindings(
                    input,
                    &port.target,
                    layout_name,
                    layout,
                    mouse,
                    &device,
                )?;
                for option_handle in layout.options.iter().copied() {
                    let option = &self.definition.options[option_handle];
                    let setting_path = SettingPath::from_key(Arc::from(format!(
                        "ports.{}.{}.{port_index}.{}",
                        self.instance.console(),
                        group_name,
                        option.key,
                        port_index = port.index,
                    )));
                    let Some(value) = self.instance.get(setting_path.as_str()) else {
                        continue;
                    };
                    let source = option.emit.value.as_ref().map_or(
                        WriteSource::Setting {
                            path: setting_path.as_str().into(),
                            value,
                            option: option_handle,
                        },
                        WriteSource::Literal,
                    );
                    self.add_emit(Some(&port.target), &option.emit, None, source)?;
                }
            }
        }
        Ok(())
    }

    fn plan_action_writes(&mut self) -> Result<()> {
        let Some(actions) = self.definition.actions.as_ref() else {
            return Ok(());
        };
        for emit in &actions.emit {
            let Some(value) = emit.value.as_ref() else {
                continue;
            };
            self.add_emit(
                Some(&actions.target),
                emit,
                None,
                WriteSource::Literal(value),
            )?;
        }
        for action in actions.map.values() {
            self.add_binding(&actions.target, &action.name, &action.expression)?;
        }
        Ok(())
    }

    fn plan_layout_bindings(
        &mut self,
        input: &InputDefinition,
        target: &'a OwnedTargetPath,
        layout_name: &str,
        layout: &LayoutDefinition,
        mouse: bool,
        device: &str,
    ) -> Result<()> {
        for (logical_key, emulator_key) in &layout.mapping {
            if let Some(mapping) = input.mouse.get(logical_key).filter(|_| mouse) {
                self.add_binding(target, emulator_key, &expand_template(mapping, device)?)?;
                continue;
            }
            let projected = match Projection::shared().target(layout_name, *logical_key) {
                Some(ProjectionTarget::Key(key)) => key,
                Some(ProjectionTarget::Transport(_) | ProjectionTarget::None) => continue,
                Some(ProjectionTarget::Both(_, _)) => {
                    return Err(Error::CompositeProjection {
                        layout: layout_name.into(),
                        key: *logical_key,
                    });
                }
                None => {
                    return Err(Error::MissingProjection {
                        layout: layout_name.into(),
                        key: *logical_key,
                    });
                }
            };
            let mapping = input
                .mapping
                .get(&projected)
                .ok_or(Error::MissingInputMapping(projected))?;
            self.add_binding(target, emulator_key, &expand_template(mapping, device)?)?;
        }
        Ok(())
    }

    fn add_binding(&mut self, target: &'a OwnedTargetPath, key: &str, value: &str) -> Result<()> {
        let handle = self
            .target_graph
            .resolve(target.as_str())
            .ok_or_else(|| Error::UnknownTarget(target.as_str().into()))?;
        self.bindings[handle.index()].push(BindingPlan {
            key: key.into(),
            value: value.into(),
        });
        Ok(())
    }

    fn add_emit(
        &mut self,
        inherited_target: Option<&'a OwnedTargetPath>,
        emit: &'a EmitDefinition,
        instance: Option<Box<str>>,
        source: WriteSource<'a>,
    ) -> Result<()> {
        let Some(target) = emit.target.as_ref().or(inherited_target) else {
            return Ok(());
        };
        let handle = self
            .target_graph
            .resolve(target.as_str())
            .ok_or_else(|| Error::UnknownTarget(target.as_str().into()))?;
        let target_definition = &self.definition.targets[handle];
        let bool_style = emit
            .bool_style
            .as_ref()
            .map(|name| {
                self.definition
                    .bool_styles
                    .get(name.as_ref())
                    .ok_or_else(|| Error::UnknownBoolStyle(name.clone()))
            })
            .transpose()?;
        self.validate_attribute(target_definition, handle, emit)?;
        self.writes[handle.index()]
            .entry(instance)
            .or_default()
            .push(WritePlan {
                key: &emit.key,
                source,
                bool_style,
                int_style: emit.int_style.as_deref(),
                attribute: emit.xml_attribute,
            });
        Ok(())
    }

    fn write_section(
        &self,
        handle: Handle<TargetDefinition>,
        parent_instance: Option<&str>,
        emitter: &mut dyn Emitter,
    ) -> Result<()> {
        let target = &self.definition.targets[handle];
        if target.repeat {
            for instance in self.writes[handle.index()]
                .keys()
                .filter_map(Option::as_deref)
                .filter(|instance| Self::belongs_to(*instance, parent_instance))
            {
                self.write_section_instance(handle, Some(instance), emitter)?;
            }
        } else {
            self.write_section_instance(handle, parent_instance, emitter)?;
        }
        Ok(())
    }

    fn write_section_instance(
        &self,
        handle: Handle<TargetDefinition>,
        instance: Option<&str>,
        emitter: &mut dyn Emitter,
    ) -> Result<()> {
        let target = &self.definition.targets[handle];
        let attributes = self
            .plans(handle, instance)
            .into_iter()
            .flatten()
            .filter(|plan| plan.attribute)
            .map(|plan| {
                self.scalar_for_plan(plan).map(|value| {
                    value.map(|value| EmittedAttribute {
                        key: plan.key,
                        value,
                    })
                })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let section = SectionRef {
            path: target.path.as_str(),
            name: target
                .path
                .components()
                .last()
                .unwrap_or(target.path.as_str()),
            attributes: &attributes,
        };
        emitter.enter_section(section)?;
        self.write_values(handle, instance, emitter)?;
        for child in self.target_graph.children(handle) {
            if self.is_active(child, instance) {
                self.write_section(child, instance, emitter)?;
            }
        }
        emitter.exit_section(section)?;
        Ok(())
    }

    fn write_values(
        &self,
        handle: Handle<TargetDefinition>,
        instance: Option<&str>,
        emitter: &mut dyn Emitter,
    ) -> Result<()> {
        for plan in self.plans(handle, instance).into_iter().flatten() {
            if plan.attribute {
                continue;
            }
            if let WriteSource::OwnedString(value) = &plan.source {
                emitter.write_value(EmittedValue {
                    key: plan.key,
                    value: ScalarValue::String(value),
                })?;
                continue;
            }
            let Some(value) = self.scalar_for_plan(plan)? else {
                continue;
            };
            emitter.write_value(EmittedValue {
                key: plan.key,
                value,
            })?;
        }
        if instance.is_none() {
            for binding in &self.bindings[handle.index()] {
                emitter.write_binding(EmittedBinding {
                    key: &binding.key,
                    value: std::borrow::Cow::Borrowed(&binding.value),
                })?;
            }
        }
        Ok(())
    }

    fn scalar_for_plan<'b>(&'b self, plan: &'b WritePlan<'a>) -> Result<Option<ScalarValue<'b>>> {
        let (value, path, option) = match &plan.source {
            WriteSource::Setting {
                path,
                value,
                option,
            } => (*value, path.as_ref(), Some(*option)),
            WriteSource::Literal(value) => (*value, "<literal>", None),
            WriteSource::OwnedString(value) => return Ok(Some(ScalarValue::String(value))),
        };
        self.scalar(value, path, option, plan)
    }

    fn plans(
        &self,
        handle: Handle<TargetDefinition>,
        instance: Option<&str>,
    ) -> Option<&[WritePlan<'a>]> {
        self.writes[handle.index()]
            .iter()
            .find(|(key, _)| key.as_deref() == instance)
            .map(|(_, plans)| plans.as_slice())
    }

    fn scalar<'b>(
        &'b self,
        value: &'a Value,
        path: &str,
        option: Option<Handle<OptionDefinition>>,
        plan: &'b WritePlan<'a>,
    ) -> Result<Option<ScalarValue<'b>>> {
        let value = match value {
            Value::Null => return Ok(None),
            Value::Choice(choice) => {
                let option = option.expect("literal emit values cannot be choices");
                let selected = self
                    .instance
                    .definition()
                    .choices(option)
                    .and_then(|choices| choices.iter().find(|item| item.key.as_str() == &**choice))
                    .ok_or_else(|| Error::UnknownChoice {
                        path: path.into(),
                        choice: choice.clone(),
                    })?;
                if matches!(selected.value, Value::Null | Value::Choice(_)) {
                    return Err(Error::NonScalarChoice {
                        path: path.into(),
                        choice: choice.clone(),
                    });
                }
                &selected.value
            }
            value => value,
        };

        Ok(Some(match value {
            Value::Boolean(value) => ScalarValue::Boolean {
                value: *value,
                style: plan.bool_style,
            },
            Value::Integer(value) => ScalarValue::Integer {
                value: *value,
                style: plan.int_style,
            },
            Value::Float(value) => ScalarValue::Float(*value),
            Value::String(value) => ScalarValue::String(value),
            Value::Path(value) => ScalarValue::Path(value),
            Value::Null | Value::Choice(_) => unreachable!(),
        }))
    }

    fn validate_repetition(&self) -> Result<()> {
        let mut repeated_sections = HashSet::new();
        for (_, target) in self.definition.targets.iter() {
            if target.repeat && target.parent.is_none() && target.additional_parents.is_empty() {
                return Err(Error::RepeatWithoutParent(target.path.as_str().into()));
            }
        }
        for (_, section) in self.definition.sections.iter() {
            let Some(target_path) = section.target.as_ref() else {
                if section.repeat {
                    return Err(Error::RepeatMismatch {
                        section: section.path.as_str().into(),
                        target: "<none>".into(),
                    });
                }
                continue;
            };
            let target = self
                .target_graph
                .resolve(target_path.as_str())
                .ok_or_else(|| Error::UnknownTarget(target_path.as_str().into()))?;
            if section.repeat != self.definition.targets[target].repeat {
                return Err(Error::RepeatMismatch {
                    section: section.path.as_str().into(),
                    target: target_path.as_str().into(),
                });
            }
            if section.repeat {
                repeated_sections.insert(target);
            }
            for option in section.options.iter().copied() {
                let emit = &self.definition.options[option].emit;
                let emit_target = emit.target.as_ref().unwrap_or(target_path);
                let emit_handle = self
                    .target_graph
                    .resolve(emit_target.as_str())
                    .ok_or_else(|| Error::UnknownTarget(emit_target.as_str().into()))?;
                self.validate_attribute(&self.definition.targets[emit_handle], emit_handle, emit)?;
            }
        }
        for (handle, target) in self.definition.targets.iter() {
            if target.repeat && !repeated_sections.contains(&handle) {
                return Err(Error::RepeatMismatch {
                    section: "<none>".into(),
                    target: target.path.as_str().into(),
                });
            }
        }
        Ok(())
    }

    fn validate_attribute(
        &self,
        target: &TargetDefinition,
        handle: Handle<TargetDefinition>,
        emit: &EmitDefinition,
    ) -> Result<()> {
        if !emit.xml_attribute {
            return Ok(());
        }
        if target.parent.is_none() && target.additional_parents.is_empty() {
            return Err(Error::AttributeOnDocumentRoot(target.path.as_str().into()));
        }
        if let Some(format) = self
            .target_graph
            .output_formats(handle)
            .iter()
            .find(|format| format.as_ref() != "xml")
        {
            return Err(Error::AttributeUnsupportedFormat {
                target: target.path.as_str().into(),
                format: format.clone(),
            });
        }
        Ok(())
    }

    fn is_active(&self, handle: Handle<TargetDefinition>, instance: Option<&str>) -> bool {
        let target = &self.definition.targets[handle];
        let own_writes = if target.repeat {
            self.writes[handle.index()]
                .keys()
                .filter_map(Option::as_deref)
                .any(|candidate| Self::belongs_to(candidate, instance))
        } else {
            self.plans(handle, instance).is_some_and(|plans| {
                plans.iter().any(|plan| match &plan.source {
                    WriteSource::Setting { value, .. } | WriteSource::Literal(value) => {
                        !matches!(value, Value::Null)
                    }
                    WriteSource::OwnedString(_) => true,
                })
            }) || instance.is_none() && !self.bindings[handle.index()].is_empty()
        };
        own_writes
            || self
                .target_graph
                .children(handle)
                .any(|child| self.is_active(child, instance))
    }

    fn belongs_to(instance: &str, parent: Option<&str>) -> bool {
        parent.is_none_or(|parent| {
            instance.starts_with(parent)
                && instance
                    .as_bytes()
                    .get(parent.len())
                    .is_some_and(|byte| *byte == b'.')
        })
    }
}

fn expand_device(template: &str, index: usize) -> Result<String> {
    let name = SdlGamepadIdentity::NAME;
    let guid = SdlGamepadIdentity::GUID.simple().to_string();
    let expanded = template
        .replace("{index}", &index.to_string())
        .replace("{name}", name)
        .replace("{guid}", &guid);
    reject_unknown_template(template, expanded)
}

fn expand_template(template: &str, device: &str) -> Result<String> {
    reject_unknown_template(template, template.replace("{device}", device))
}

fn reject_unknown_template(original: &str, expanded: String) -> Result<String> {
    if expanded.contains(['{', '}']) {
        Err(Error::UnknownTemplateField(original.into()))
    } else {
        Ok(expanded)
    }
}
