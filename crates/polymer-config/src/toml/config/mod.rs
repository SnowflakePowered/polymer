use std::collections::BTreeMap;

use toml_spanner::Toml;

mod options;
mod path;

pub use options::*;

pub use path::TargetPath;

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Target<'de> {
    pub output: Option<TargetOutput<'de>>,
    #[toml(default)]
    pub repeat: bool,
    #[toml(default)]
    pub parent: Vec<TargetPath<'de>>,
    #[toml(flatten)]
    pub children: BTreeMap<&'de str, Target<'de>>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct TargetOutput<'de> {
    pub format: &'de str,
    pub path: &'de str,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct BoolStyle<'de> {
    #[toml(rename = "true")]
    pub r#true: &'de str,
    #[toml(rename = "false")]
    pub r#false: &'de str,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Section<'de> {
    pub title: &'de str,
    pub target: Option<TargetPath<'de>>,
    #[toml(default)]
    pub repeat: bool,
    #[toml(default)]
    pub options: Vec<OptionDefinition<'de>>,
    #[toml(flatten)]
    pub sections: BTreeMap<&'de str, Section<'de>>,
}
