use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use crate::{
    Handle,
    ast::{
        BoolStyleDefinition, ConfigurationDefinition, ConfigurationInstance, EmitDefinition,
        OptionDefinition, OwnedTargetPath, SettingPath, TargetDefinition, TargetGraph,
        TargetOutputDefinition, Value, family,
    },
    error::{Error, Result},
};

use super::{DocumentRef, EmittedBinding, EmittedValue, Emitter, ScalarValue, SectionRef};
use crate::input::{
    gamepad::VirtualPad,
    projection::{Projection, Target as ProjectionTarget},
};

/// The compiled serialization state for one configuration instance.
pub struct Serializer<'a> {
    instance: &'a ConfigurationInstance,
    definition: &'a ConfigurationDefinition,
    target_graph: &'a TargetGraph,
    writes: Vec<Vec<WritePlan<'a>>>,
    bindings: Vec<Vec<BindingPlan>>,
    active: Vec<bool>,
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
        let definition = instance.definition().definition();
        let target_graph = definition.target_graph()?;

        let mut serializer = Self {
            instance,
            definition,
            target_graph,
            writes: (0..definition.targets.len()).map(|_| Vec::new()).collect(),
            bindings: (0..definition.targets.len()).map(|_| Vec::new()).collect(),
            active: vec![false; definition.targets.len()],
            gamepad_bindings,
        };
        serializer.plan_section_writes()?;
        serializer.plan_input_writes()?;
        serializer.plan_port_writes()?;
        if let Some(((group, index), _)) = serializer.gamepad_bindings.first_key_value() {
            return Err(Error::UnexpectedGamepadBinding {
                group: group.clone(),
                index: *index,
            });
        }
        serializer.compute_active_targets(target_graph.topological_order());
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
        self.write_values(document.target, emitter.as_mut())?;
        for child in self.target_graph.children(document.target) {
            if self.active[child.index()] {
                self.write_section(child, emitter.as_mut())?;
            }
        }
        emitter.end_document(document_ref)?;
        let mut bytes = Vec::new();
        emitter.finish(&mut bytes)?;
        Ok(bytes)
    }

    /// Serializes every output-bearing root into a path-to-bytes map.
    pub fn serialize(
        &self,
        mut emitter_for: impl FnMut(&str) -> Option<Box<dyn Emitter>>,
    ) -> Result<BTreeMap<Box<str>, Vec<u8>>> {
        let mut outputs = BTreeMap::new();
        for document in self.documents() {
            if !self.active[document.target.index()] {
                continue;
            }
            if outputs.contains_key(document.output.path.as_ref()) {
                return Err(Error::DuplicateOutputPath(document.output.path.clone()));
            }
            let emitter = emitter_for(&document.output.format)
                .ok_or_else(|| Error::UnsupportedFormat(document.output.format.clone()))?;
            let bytes = self.serialize_document(document, emitter)?;
            outputs.insert(document.output.path.clone(), bytes);
        }
        Ok(outputs)
    }

    fn plan_section_writes(&mut self) -> Result<()> {
        for (_, section) in self.definition.sections.iter() {
            for option_handle in section.options.iter().copied() {
                let option = &self.definition.options[option_handle];
                let setting_path = section.path.join::<family::Setting>(&option.key);
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
                self.add_emit(section.target.as_ref(), &option.emit, source)?;
            }
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
            self.add_emit(None, &option.emit, source)?;
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
                let layout_name = assignments
                    .and_then(|assignments| assignments.get(port.index))
                    .map_or("unplugged", AsRef::as_ref);

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
                        self.add_emit(Some(&port.target), emit, source)?;
                    }
                }

                let Some(layout) = layout else {
                    continue;
                };
                let device = device.expect("a known layout has a virtual device");
                for (logical_key, emulator_key) in &layout.mapping {
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
                    self.add_binding(
                        &port.target,
                        emulator_key,
                        &expand_template(mapping, &device)?,
                    )?;
                }
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
                    self.add_emit(Some(&port.target), &option.emit, source)?;
                }
            }
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
        source: WriteSource<'a>,
    ) -> Result<()> {
        let Some(target) = emit.target.as_ref().or(inherited_target) else {
            return Ok(());
        };
        let handle = self
            .target_graph
            .resolve(target.as_str())
            .ok_or_else(|| Error::UnknownTarget(target.as_str().into()))?;
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
        self.writes[handle.index()].push(WritePlan {
            key: &emit.key,
            source,
            bool_style,
            int_style: emit.int_style.as_deref(),
        });
        Ok(())
    }

    fn write_section(
        &self,
        handle: Handle<TargetDefinition>,
        emitter: &mut dyn Emitter,
    ) -> Result<()> {
        let target = &self.definition.targets[handle];
        let section = SectionRef {
            path: target.path.as_str(),
            name: target
                .path
                .components()
                .last()
                .unwrap_or(target.path.as_str()),
        };
        emitter.enter_section(section)?;
        self.write_values(handle, emitter)?;
        for child in self.target_graph.children(handle) {
            if self.active[child.index()] {
                self.write_section(child, emitter)?;
            }
        }
        emitter.exit_section(section)?;
        Ok(())
    }

    fn write_values(
        &self,
        handle: Handle<TargetDefinition>,
        emitter: &mut dyn Emitter,
    ) -> Result<()> {
        for plan in &self.writes[handle.index()] {
            if let WriteSource::OwnedString(value) = &plan.source {
                emitter.write_value(EmittedValue {
                    key: plan.key,
                    value: ScalarValue::String(value),
                })?;
                continue;
            }
            let (value, path, option) = match &plan.source {
                WriteSource::Setting {
                    path,
                    value,
                    option,
                } => (*value, path.as_ref(), Some(*option)),
                WriteSource::Literal(value) => (*value, "<literal>", None),
                WriteSource::OwnedString(_) => unreachable!(),
            };
            let Some(value) = self.scalar(value, path, option, plan)? else {
                continue;
            };
            emitter.write_value(EmittedValue {
                key: plan.key,
                value,
            })?;
        }
        for binding in &self.bindings[handle.index()] {
            emitter.write_binding(EmittedBinding {
                key: &binding.key,
                value: std::borrow::Cow::Borrowed(&binding.value),
            })?;
        }
        Ok(())
    }

    fn scalar(
        &self,
        value: &'a Value,
        path: &str,
        option: Option<Handle<OptionDefinition>>,
        plan: &WritePlan<'a>,
    ) -> Result<Option<ScalarValue<'a>>> {
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

    fn compute_active_targets(&mut self, target_order: &[Handle<TargetDefinition>]) {
        for handle in target_order.iter().rev().copied() {
            self.active[handle.index()] = !self.bindings[handle.index()].is_empty()
                || self.writes[handle.index()]
                    .iter()
                    .any(|write| match &write.source {
                        WriteSource::Setting { value, .. } | WriteSource::Literal(value) => {
                            !matches!(value, Value::Null)
                        }
                        WriteSource::OwnedString(_) => true,
                    })
                || self
                    .target_graph
                    .children(handle)
                    .any(|child| self.active[child.index()]);
        }
    }
}

fn expand_device(template: &str, index: usize) -> Result<String> {
    let name = VirtualPad::GAMEPAD_NAME;
    let guid = VirtualPad::GAMEPAD_GUID.simple().to_string();
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

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, fmt::Write as FmtWrite, io::Write as IoWrite, sync::Arc};

    use crate::{
        ConfigurationToml,
        ast::{
            ConfigurationDefinition, DefaultDefinition, DomainDefinition, RealizedDomain,
            RealizedOption, Value, ValueKind,
        },
        ser::{BmlEmitter, EmitResult, IniEmitter, XmlEmitter},
    };

    use super::*;

    #[derive(Default)]
    struct RecordingEmitter {
        output: String,
    }

    impl Emitter for RecordingEmitter {
        fn begin_document(&mut self, document: DocumentRef<'_>) -> EmitResult<()> {
            writeln!(self.output, "document {}", document.target).map_err(Error::emitter)
        }

        fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
            writeln!(self.output, "enter {}", section.name).map_err(Error::emitter)
        }

        fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
            match value.value {
                ScalarValue::Boolean {
                    value: boolean,
                    style: Some(style),
                } => writeln!(
                    self.output,
                    "{}={}",
                    value.key,
                    if boolean {
                        &style.r#true
                    } else {
                        &style.r#false
                    }
                ),
                ScalarValue::String(string) => {
                    writeln!(self.output, "{}={string}", value.key)
                }
                ScalarValue::Path(path) => {
                    writeln!(self.output, "{}={}", value.key, path.display())
                }
                ScalarValue::Integer { value: integer, .. } => {
                    writeln!(self.output, "{}={integer}", value.key)
                }
                other => writeln!(self.output, "{}={other:?}", value.key),
            }
            .map_err(Error::emitter)
        }

        fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
            writeln!(self.output, "exit {}", section.name).map_err(Error::emitter)
        }

        fn end_document(&mut self, _document: DocumentRef<'_>) -> EmitResult<()> {
            writeln!(self.output, "end").map_err(Error::emitter)
        }

        fn finish(&mut self, writer: &mut dyn IoWrite) -> EmitResult<()> {
            writer.write_all(self.output.as_bytes())?;
            Ok(())
        }
    }

    #[test]
    fn lowers_realizes_and_serializes_top_down() {
        let source = r#"
            [bool_style.words]
            true = "yes"
            false = "no"

            [targets.main]
            output = { format = "recording", path = "config.txt" }
            [targets.main.Core]
            [targets.main.Unused]

            [sections.general]
            title = "General"
            target = "main"

            [[sections.general.options]]
            key = "enabled"
            title = "Enabled"
            type = "bool"
            default = false
            emit = { key = "Enabled", bool_style = "words" }

            [[sections.general.options]]
            key = "data_path"
            title = "Data Path"
            type = "path"
            default = "/default/data"
            emit = { key = "DataPath" }

            [sections.graphics]
            title = "Graphics"
            target = "main.Core"

            [[sections.graphics.options]]
            key = "backend"
            title = "Backend"
            type = "select"
            default = "vulkan"
            domain = [{ key = "vulkan", label = "Vulkan", value = "VulkanAPI" }]
            emit = { key = "Backend" }

            [input]
            driver = "test"
            device = "test/{index}"
            [input.mapping]

            [[input.mapping.options]]
            key = "gain"
            title = "Input Gain"
            type = "int"
            default = 3
            emit = { target = "main.Core", key = "InputGain" }

            [input.layouts.pad]

            [platforms.example.ports.controller.0]
            target = "main.Core"
            layouts = ["pad"]
            emit.unplugged = [{ key = "Port", value = "none" }]
        "#;
        let definition = {
            let arena = toml_spanner::Arena::new();
            let parsed = ConfigurationToml::try_from_tomls(&arena, [source].into_iter()).unwrap();
            Arc::new(ConfigurationDefinition::from(parsed))
        };
        let realized = definition
            .resolve_with(|_, option| -> std::result::Result<_, Infallible> {
                let domain = match &option.value.kind {
                    ValueKind::Select { domain } => {
                        if matches!(domain, DomainDefinition::Static(_)) {
                            RealizedDomain::Static
                        } else {
                            RealizedDomain::None
                        }
                    }
                    _ => RealizedDomain::None,
                };
                Ok(RealizedOption {
                    default: literal(&option.value.default),
                    domain,
                })
            })
            .unwrap();
        let mut instance = realized.instantiate("example", BTreeMap::new()).unwrap();
        instance["general.enabled"] = Value::Boolean(true);
        assert_eq!(
            instance["general.data_path"],
            Value::Path(std::path::PathBuf::from("/default/data"))
        );
        instance["general.data_path"] = Value::Path("/games/library".into());
        assert_eq!(instance["input.gain"], Value::Integer(3));
        instance["input.gain"] = Value::Integer(4);

        let serializer = Serializer::new(&instance, []).unwrap();
        assert_eq!(serializer.documents().count(), 1);
        let outputs = serializer
            .serialize(|format| {
                (format == "recording")
                    .then(|| Box::new(RecordingEmitter::default()) as Box<dyn Emitter>)
            })
            .unwrap();

        assert_eq!(
            std::str::from_utf8(&outputs["config.txt"]).unwrap(),
            "document main\nEnabled=yes\nDataPath=/games/library\nenter Core\nBackend=VulkanAPI\nInputGain=4\nPort=none\nexit Core\nend\n"
        );

        let ini_outputs = serializer
            .serialize(|_| Some(Box::new(IniEmitter::new()) as Box<dyn Emitter>))
            .unwrap();
        let mut ini = configparser::ini::Ini::new_cs();
        ini.read(String::from_utf8(ini_outputs["config.txt"].clone()).unwrap())
            .unwrap();
        assert_eq!(ini.get("default", "Enabled").as_deref(), Some("yes"));
        assert_eq!(
            ini.get("default", "DataPath").as_deref(),
            Some("/games/library")
        );
        assert_eq!(ini.get("Core", "Backend").as_deref(), Some("VulkanAPI"));
        assert_eq!(ini.get("Core", "InputGain").as_deref(), Some("4"));
        assert_eq!(ini.get("Core", "Port").as_deref(), Some("none"));
    }

    #[test]
    fn serializes_projected_inputs_for_supported_emulator_shapes() {
        let cemu = serialize_plugin(
            &[include_str!("../../../../plugins/cemu/config/input.toml")],
            "NINTENDO_WIIU",
            "gamepad",
            "WIIU_DRC_CONTROLLER",
        );
        let cemu = text(&cemu["controllerProfiles/controller0.xml"]);
        assert!(cemu.contains("<api>SDLController</api>"), "{cemu}");
        assert!(
            cemu.contains("<uuid>0_ff009378536e6f77676c617373007601</uuid>"),
            "{cemu}"
        );
        assert!(cemu.contains("<mapping>1</mapping>"), "{cemu}");
        assert!(cemu.contains("<button>0</button>"), "{cemu}");

        let dolphin = serialize_plugin(
            &[
                include_str!("../../../../plugins/dolphin/config/dolphin.toml"),
                include_str!("../../../../plugins/dolphin/config/input.toml"),
            ],
            "NINTENDO_GCN",
            "controller",
            "GAMECUBE_CONTROLLER",
        );
        let dolphin = text(&dolphin["Config/GCPadNew.ini"]);
        assert!(
            dolphin.contains("Device=SDL/0/Snowglass Virtual Gamepad"),
            "{dolphin}"
        );
        assert!(dolphin.contains("Buttons/A=`Button S`"), "{dolphin}");

        let pcsx2 = serialize_plugin(
            &[include_str!("../../../../plugins/pcsx2/config/input.toml")],
            "SONY_PS2",
            "controller",
            "DUALSHOCK_2_CONTROLLER",
        );
        let pcsx2 = text(&pcsx2["inis/PCSX2.ini"]);
        assert!(pcsx2.contains("Type=DualShock2"), "{pcsx2}");
        assert!(pcsx2.contains("Cross=SDL-0/FaceSouth"), "{pcsx2}");

        let ares = serialize_plugin(
            &[include_str!("../../../../plugins/ares/config/input.toml")],
            "ATARI_2600",
            "controller",
            "A2600_JOYSTICK_CONTROLLER",
        );
        let ares = text(&ares["settings.bml"]);
        assert!(
            ares.contains("A..South: ff009378536e6f77676c617373007601/0/3/0"),
            "{ares}"
        );
    }

    fn serialize_plugin(
        sources: &[&str],
        console: &str,
        group: &str,
        layout: &str,
    ) -> BTreeMap<Box<str>, Vec<u8>> {
        let arena = toml_spanner::Arena::new();
        let parsed = ConfigurationToml::try_from_tomls(&arena, sources.iter().copied()).unwrap();
        let definition = Arc::new(ConfigurationDefinition::from(parsed));
        let realized = definition
            .resolve_with(|_, option| -> std::result::Result<_, Infallible> {
                let domain = match &option.value.kind {
                    ValueKind::Select { domain } => {
                        if matches!(domain, DomainDefinition::Static(_)) {
                            RealizedDomain::Static
                        } else {
                            RealizedDomain::None
                        }
                    }
                    _ => RealizedDomain::None,
                };
                Ok(RealizedOption {
                    default: literal(&option.value.default),
                    domain,
                })
            })
            .unwrap();
        let assignments = BTreeMap::from([(group.into(), vec![layout.into()].into_boxed_slice())]);
        let instance = realized.instantiate(console, assignments).unwrap();
        Serializer::new(
            &instance,
            [GamepadBinding {
                group: group.into(),
                port: 0,
                gamepad: 0,
            }],
        )
        .unwrap()
        .serialize(|format| match format {
            "ini" => Some(Box::new(IniEmitter::new())),
            "xml" => Some(Box::new(XmlEmitter::new())),
            "bml" => Some(Box::new(BmlEmitter::new())),
            _ => None,
        })
        .unwrap()
    }

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).unwrap()
    }

    fn literal(default: &DefaultDefinition) -> Value {
        match default {
            DefaultDefinition::Literal(value) => value.clone(),
            DefaultDefinition::None | DefaultDefinition::Resolver(_) => Value::Null,
        }
    }
}
