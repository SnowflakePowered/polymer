use std::collections::BTreeMap;

use consolespec::BindingKey;
use indexmap::IndexMap;

use crate::arena::{Arena, Handle};

use super::{ChoiceKey, OwnedTargetPath, SectionPath, Value};

/// An owned, context-independent configuration definition.
#[derive(Debug)]
pub struct ConfigurationDefinition {
    pub meta: BTreeMap<Box<str>, Box<str>>,
    pub bool_styles: BTreeMap<Box<str>, BoolStyleDefinition>,
    pub targets: Arena<TargetDefinition>,
    pub root_targets: Box<[Handle<TargetDefinition>]>,
    pub sections: Arena<SectionDefinition>,
    pub root_sections: Box<[Handle<SectionDefinition>]>,
    pub options: Arena<OptionDefinition>,
    pub input: Option<InputDefinition>,
}

impl ConfigurationDefinition {
    pub fn new() -> Self {
        Self {
            meta: BTreeMap::new(),
            bool_styles: BTreeMap::new(),
            targets: Arena::new(),
            root_targets: Box::new([]),
            sections: Arena::new(),
            root_sections: Box::new([]),
            options: Arena::new(),
            input: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BoolStyleDefinition {
    pub r#true: Box<str>,
    pub r#false: Box<str>,
}

#[derive(Clone, Debug)]
pub struct TargetDefinition {
    pub path: OwnedTargetPath,
    pub output: Option<TargetOutputDefinition>,
    pub parent: Option<Handle<TargetDefinition>>,
    pub additional_parents: Box<[OwnedTargetPath]>,
    pub children: Box<[Handle<TargetDefinition>]>,
}

#[derive(Clone, Debug)]
pub struct TargetOutputDefinition {
    pub format: Box<str>,
    pub path: Box<str>,
}

impl Default for ConfigurationDefinition {
    fn default() -> Self {
        Self::new()
    }
}

/// A node in the flattened presentation hierarchy.
#[derive(Clone, Debug)]
pub struct SectionDefinition {
    pub key: Box<str>,
    pub path: SectionPath,
    pub title: Box<str>,
    pub description: Option<Box<str>>,
    pub target: Option<OwnedTargetPath>,
    pub parent: Option<Handle<SectionDefinition>>,
    pub children: Box<[Handle<SectionDefinition>]>,
    pub options: Box<[Handle<OptionDefinition>]>,
}

/// Static metadata and constraints shared by every instance of an option.
#[derive(Clone, Debug)]
pub struct OptionDefinition {
    /// The local key beneath a section or concrete input placement.
    pub key: Box<str>,
    pub title: Box<str>,
    pub description: Option<Box<str>>,
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
}

#[derive(Clone, Debug)]
pub enum ValueDefinition {
    Boolean {
        default: DefaultDefinition,
    },
    Integer {
        range: Option<OptionRange>,
        default: DefaultDefinition,
    },
    Float {
        range: Option<OptionRange>,
        default: DefaultDefinition,
    },
    String {
        default: DefaultDefinition,
    },
    Select {
        domain: DomainDefinition,
        default: DefaultDefinition,
    },
}

#[derive(Clone, Debug)]
pub enum DefaultDefinition {
    None,
    Literal(Value),
    Resolver(ResolverSource),
}

#[derive(Clone, Debug)]
pub enum DomainDefinition {
    None,
    Static(Box<[Choice]>),
    Resolver(ResolverSource),
}

#[derive(Clone, Debug)]
pub struct ResolverSource(Box<str>);

impl ResolverSource {
    pub fn new(source: impl Into<Box<str>>) -> Self {
        Self(source.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    pub options: Box<[Handle<OptionDefinition>]>,
    pub layouts: BTreeMap<Box<str>, LayoutDefinition>,
    pub platforms: BTreeMap<Box<str>, PlatformDefinition>,
}

#[derive(Debug)]
pub struct LayoutDefinition {
    pub mapping: IndexMap<BindingKey, Box<str>>,
    pub options: Box<[Handle<OptionDefinition>]>,
}

#[derive(Debug)]
pub struct PlatformDefinition {
    pub groups: BTreeMap<Box<str>, PortGroupDefinition>,
}

#[derive(Debug)]
pub struct PortGroupDefinition {
    /// Array position is the port index exposed through the launch API.
    pub ports: Box<[PortDefinition]>,
}

#[derive(Debug)]
pub struct PortDefinition {
    pub index: usize,
    pub target: OwnedTargetPath,
    pub layouts: Box<[Box<str>]>,
    pub emit: BTreeMap<Box<str>, Box<[EmitDefinition]>>,
}

/// A lightweight reference joining an option's static and realized rows.
pub struct RealizedOptionRef<'a> {
    pub definition: &'a OptionDefinition,
    pub realized: &'a super::RealizedOption,
}
