use std::collections::BTreeMap;

use consolespec::input::BindingKey;
use indexmap::IndexMap;
use toml_spanner::Toml;

use crate::toml::config::{OptionDefinition, TargetPath};

use super::config::Emit;

/// The raw TOML `input` namespace.
///
/// Contains the definitions for an input configuration in
/// an input.toml.
#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct InputBlock<'de> {
    /// The driver to hook for this emulator.
    pub driver: &'de str,
    /// The device format string.
    pub device: &'de str,
    /// The mapping **values** from the SDL virtual gamepad to the emulator's
    /// configuration dialect.
    pub mapping: LayoutMapping<'de>,
    pub mouse: Option<MouseMapping<'de>>,
    /// The mapping **keys** from the layout inputspec to the emulator's
    /// emulated controller keys.
    pub layouts: BTreeMap<&'de str, LayoutMapping<'de>>,
}

/// The raw TOML `input.layouts` namespace.
///
/// A mapping from an inputspec binding key expression
/// to some value.
#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct LayoutMapping<'de> {
    #[toml(flatten, with = super::with::binding_map)]
    pub mapping: IndexMap<BindingKey, &'de str>,
    #[toml(default)]
    pub options: Vec<OptionDefinition<'de>>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct MouseMapping<'de> {
    #[toml(flatten, with = super::with::binding_map)]
    pub mapping: IndexMap<BindingKey, &'de str>,
}

pub type PortGroup<'de> = BTreeMap<&'de str, PortAssignment<'de>>;
pub type PortGroups<'de> = BTreeMap<&'de str, PortGroup<'de>>;

/// The ports one machine exposes.
///
/// A struct rather than another map level so that a misspelled `ports` is
/// rejected where it is written, instead of parsing as a platform whose only
/// input group is named after the typo.
#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct PortTable<'de> {
    pub ports: PortGroups<'de>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct PortAssignment<'de> {
    pub target: TargetPath<'de>,
    pub layouts: Vec<&'de str>,
    pub emit: BTreeMap<&'de str, Vec<Emit<'de>>>,
}
