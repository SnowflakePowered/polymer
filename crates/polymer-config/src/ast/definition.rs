use std::{collections::BTreeMap, sync::OnceLock};

use consolespec::input::BindingKey;
use indexmap::IndexMap;

use crate::arena::{Arena, Handle};

use super::{ChoiceKey, OwnedTargetPath, SectionPath, TargetDefinition, TargetGraph, Value};

/// An owned, context-independent configuration definition.
#[derive(Debug)]
pub struct ConfigurationDefinition {
    pub meta: BTreeMap<Box<str>, Box<str>>,
    pub bool_styles: BTreeMap<Box<str>, BoolStyleDefinition>,
    pub targets: Arena<TargetDefinition>,
    pub sections: Arena<SectionDefinition>,
    pub root_sections: Box<[Handle<SectionDefinition>]>,
    pub options: Arena<OptionDefinition>,
    pub actions: Option<ActionsDefinition>,
    pub input: Option<InputDefinition>,
    pub(super) target_graph: OnceLock<TargetGraph>,
}

impl ConfigurationDefinition {
    pub fn new() -> Self {
        Self {
            meta: BTreeMap::new(),
            bool_styles: BTreeMap::new(),
            targets: Arena::new(),
            sections: Arena::new(),
            root_sections: Box::new([]),
            options: Arena::new(),
            actions: None,
            input: None,
            target_graph: OnceLock::new(),
        }
    }

    pub fn option(&self, handle: Handle<OptionDefinition>) -> &OptionDefinition {
        &self.options[handle]
    }

    pub fn choices(&self, handle: Handle<OptionDefinition>) -> Option<&[Choice]> {
        match &self.options[handle].value.kind {
            ValueKind::Select {
                domain: DomainDefinition::Static(choices),
            } => Some(choices),
            _ => None,
        }
    }

    pub fn root_sections(&self) -> impl ExactSizeIterator<Item = Handle<SectionDefinition>> + '_ {
        self.root_sections.iter().copied()
    }
}

#[derive(Clone, Debug)]
pub struct BoolStyleDefinition {
    pub r#true: Box<str>,
    pub r#false: Box<str>,
}

impl Default for ConfigurationDefinition {
    fn default() -> Self {
        Self::new()
    }
}

/// A node in the flattened presentation hierarchy.
#[derive(Clone, Debug)]
pub struct SectionDefinition {
    pub path: SectionPath,
    pub title: Box<str>,
    pub target: Option<OwnedTargetPath>,
    pub repeat: bool,
    pub children: Box<[Handle<SectionDefinition>]>,
    pub options: Box<[Handle<OptionDefinition>]>,
}

/// Static metadata and constraints shared by every instance of an option.
#[derive(Clone, Debug)]
pub struct OptionDefinition {
    /// The local key beneath a section or concrete input placement.
    pub key: Box<str>,
    pub title: Box<str>,
    pub value: ValueDefinition,
    pub emit: EmitDefinition,
}

#[derive(Clone, Debug)]
pub struct EmitDefinition {
    pub target: Option<OwnedTargetPath>,
    pub key: Box<str>,
    pub value: Option<Value>,
    pub bool_style: Option<Box<str>>,
    pub int_style: Option<Box<str>>,
    pub xml_attribute: bool,
}

#[derive(Clone, Debug)]
pub struct ValueDefinition {
    pub kind: ValueKind,
    pub default: DefaultDefinition,
}

#[derive(Clone, Debug)]
pub enum ValueKind {
    Boolean,
    Integer { range: Option<OptionRange> },
    Float { range: Option<OptionRange> },
    String,
    Path,
    Select { domain: DomainDefinition },
}

#[derive(Clone, Debug)]
pub enum DefaultDefinition {
    None,
    Literal(Value),
}

#[derive(Clone, Debug)]
pub enum DomainDefinition {
    None,
    Static(Box<[Choice]>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub key: ChoiceKey,
    pub label: Box<str>,
    /// The emulator-facing value retained by the definition for later output.
    pub value: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OptionRange {
    Integer { min: i64, max: i64 },
    Float { min: f64, max: f64 },
}

/// Input option templates and the consoles on which they may be instantiated.
#[derive(Debug)]
pub struct InputDefinition {
    pub driver: Box<str>,
    pub device: Box<str>,
    pub mapping: IndexMap<BindingKey, Box<str>>,
    pub mouse: IndexMap<BindingKey, Box<str>>,
    pub options: Box<[Handle<OptionDefinition>]>,
    pub layouts: BTreeMap<Box<str>, LayoutDefinition>,
    pub platforms: BTreeMap<Box<str>, PlatformDefinition>,
}

#[derive(Debug)]
pub struct LayoutDefinition {
    pub mapping: IndexMap<BindingKey, Box<str>>,
    pub options: Box<[Handle<OptionDefinition>]>,
}

pub type PlatformDefinition = BTreeMap<Box<str>, PortGroupDefinition>;

/// Ports in one named launch-API group.
pub type PortGroupDefinition = Box<[PortDefinition]>;

#[derive(Debug)]
pub struct PortDefinition {
    pub index: usize,
    pub target: OwnedTargetPath,
    pub layouts: Box<[Box<str>]>,
    pub emit: BTreeMap<Box<str>, Box<[EmitDefinition]>>,
}

#[derive(Clone, Debug)]
pub struct ActionsDefinition {
    pub target: OwnedTargetPath,
    pub emit: Box<[EmitDefinition]>,
    pub map: BTreeMap<Box<str>, ActionDefinition>,
}

#[derive(Clone, Debug)]
pub struct ActionDefinition {
    pub name: Box<str>,
    pub expression: Box<str>,
}
