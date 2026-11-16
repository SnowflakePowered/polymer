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
    pub float_style: Option<Box<str>>,
    pub attribute: bool,
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

pub(crate) enum ValueMismatch {
    Kind(&'static str),
    Range {
        value: String,
        min: String,
        max: String,
    },
    Choice(Box<str>),
}

impl ValueKind {
    pub(crate) fn parse_json(&self, value: &serde_json::Value) -> Result<Value, ValueMismatch> {
        if value.is_null() {
            return Ok(Value::Null);
        }
        let kind = ValueMismatch::Kind;
        match self {
            Self::Boolean => value.as_bool().map(Value::Boolean).ok_or(kind("a boolean")),
            Self::Integer { range } => {
                let number = value.as_i64().ok_or(kind("an integer"))?;
                match range {
                    Some(OptionRange::Integer { min, max }) if number < *min || number > *max => {
                        Err(ValueMismatch::Range {
                            value: number.to_string(),
                            min: min.to_string(),
                            max: max.to_string(),
                        })
                    }
                    _ => Ok(Value::Integer(number)),
                }
            }
            Self::Float { range } => {
                let number = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or(kind("a finite number"))?;
                match range {
                    Some(OptionRange::Float { min, max }) if number < *min || number > *max => {
                        Err(ValueMismatch::Range {
                            value: number.to_string(),
                            min: min.to_string(),
                            max: max.to_string(),
                        })
                    }
                    _ => Ok(Value::Float(number)),
                }
            }
            Self::String => value
                .as_str()
                .map(|value| Value::String(value.into()))
                .ok_or(kind("a string")),
            Self::Path => value
                .as_str()
                .map(|value| Value::Path(value.into()))
                .ok_or(kind("a path string")),
            Self::Select { domain } => {
                let choice = value.as_str().ok_or(kind("a choice key"))?;
                let known = match domain {
                    DomainDefinition::Static(choices) => choices
                        .iter()
                        .any(|candidate| candidate.key.as_str() == choice),
                    DomainDefinition::None => false,
                };
                if known {
                    Ok(Value::Choice(choice.into()))
                } else {
                    Err(ValueMismatch::Choice(choice.into()))
                }
            }
        }
    }
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

/// An emulator-facing binding value whose text is a template until serialized.
#[derive(Clone, Debug, PartialEq)]
pub enum BindingValue {
    Text(Box<str>),
    Integer(i64),
    List(Box<[Box<str>]>),
}

impl BindingValue {
    /// Applies `expand` to every piece of text in the value.
    pub fn map_text<E>(&self, expand: impl Fn(&str) -> Result<String, E>) -> Result<Self, E> {
        Ok(match self {
            Self::Text(text) => Self::Text(expand(text)?.into_boxed_str()),
            Self::Integer(value) => Self::Integer(*value),
            Self::List(items) => Self::List(
                items
                    .iter()
                    .map(|item| expand(item).map(String::into_boxed_str))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

/// One binding written as its own section of ordered values.
#[derive(Debug)]
pub struct BindingShape {
    pub section: Box<str>,
    pub values: IndexMap<Box<str>, BindingField>,
}

#[derive(Debug)]
pub enum BindingField {
    Key,
    Value,
    Literal(Box<str>),
}

/// Input option templates and the consoles on which they may be instantiated.
#[derive(Debug)]
pub struct InputDefinition {
    pub driver: Box<str>,
    pub device: Box<str>,
    pub mapping: IndexMap<BindingKey, BindingValue>,
    pub mouse: IndexMap<BindingKey, BindingValue>,
    pub binding: Option<BindingShape>,
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
