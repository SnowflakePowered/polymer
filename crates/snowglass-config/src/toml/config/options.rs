use super::path::TargetPath;
use toml_spanner::Toml;

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Emit<'de> {
    target: Option<TargetPath<'de>>,
    key: &'de str,
    value: Option<OptionValue<'de>>,
    bool_style: Option<&'de str>,
    int_style: Option<&'de str>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct OptionDefinition<'de> {
    pub key: &'de str,
    pub title: &'de str,
    #[toml(rename = "type")]
    pub kind: &'de str,
    pub range: Option<OptionRange>,
    pub default: Option<OptionValue<'de>>,
    pub domain: Option<OptionDomain<'de>>,
    pub emit: Emit<'de>,
}

#[derive(Debug, Toml)]
#[toml(FromToml, untagged)]
pub enum OptionDomain<'de> {
    Select(Vec<SelectDefinition<'de>>),
    Resolve { source: &'de str },
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct SelectDefinition<'de> {
    pub label: &'de str,
    pub key: &'de str,
    pub value: OptionValue<'de>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct OptionRange {
    min: i64,
    max: i64,
}

#[derive(Debug, Toml)]
#[toml(FromToml, untagged)]
pub enum OptionValue<'de> {
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(&'de str),
}
