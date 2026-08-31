use std::collections::BTreeMap;

use toml_spanner::Toml;

use super::config::{Emit, TargetPath};

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Actions<'de> {
    pub target: TargetPath<'de>,
    #[toml(default)]
    pub emit: Vec<Emit<'de>>,
    #[toml(default)]
    pub map: BTreeMap<&'de str, Action<'de>>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Action<'de> {
    pub name: &'de str,
    pub expression: &'de str,
}
