use std::collections::BTreeMap;

use toml_spanner::Toml;

mod path;

pub use path::TargetPath;

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct TargetsBlock<'de> {
    pub targets: BTreeMap<&'de str, Target<'de>>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Target<'de> {
    /// The format of the terminal target
    pub format: &'de str,
    /// The path of the terminal target to write the
    /// compiled string to.
    pub path: &'de str,
    /// Subtarget paths
    #[toml(flatten, with = super::with::target_paths)]
    pub subtargets: Vec<TargetPath<'de>>,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct BoolStyle<'de> {
    pub r#true: &'de str,
    pub r#false: &'de str,
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Emit<'de> {
    target: Option<TargetPath<'de>>,
    key: &'de str,
    value: Option<OptionValue<'de>>,
    bool_style: Option<&'de str>,
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
    pub domain: Option<Vec<SelectDefinition<'de>>>,
    pub emit: Emit<'de>,
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
    String(&'de str),
}

#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct Section<'de> {
    pub title: &'de str,
    pub target: Option<TargetPath<'de>>,
    #[toml(default)]
    pub options: Vec<OptionDefinition<'de>>,
    #[toml(flatten)]
    pub sections: BTreeMap<&'de str, Section<'de>>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use toml_spanner::{Arena, Toml};

    use super::{Section, TargetPath};

    #[derive(Toml)]
    #[toml(FromToml)]
    struct Document<'de> {
        target: TargetPath<'de>,
    }

    #[test]
    fn parses_dotted_target_path() {
        let arena = Arena::new();
        let mut document =
            toml_spanner::parse("target = \"profile-1.emulated_controller\"", &arena).unwrap();

        let parsed = document.to::<Document>().unwrap();
        assert_eq!(
            parsed.target.components().collect::<Vec<_>>(),
            ["profile-1", "emulated_controller"]
        );
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct SectionsDocument<'de> {
        sections: BTreeMap<&'de str, Section<'de>>,
    }

    #[test]
    fn parses_dolphin_section_hierarchy() {
        let arena = Arena::new();
        let mut document = toml_spanner::parse(
            r#"
                [sections.main]
                title = "Dolphin"

                [sections.main.analytics]
                title = "Analytics"
                target = "main.Analytics"
            "#,
            &arena,
        )
        .unwrap();

        let sections = document.to::<SectionsDocument>().unwrap();
        let main = &sections.sections["main"];
        assert_eq!(main.title, "Dolphin");
        assert_eq!(main.sections["analytics"].title, "Analytics");
    }

    #[test]
    fn parses_cemu_section_options() {
        let arena = Arena::new();
        let mut document = toml_spanner::parse(
            include_str!("../../../../plugins/cemu/config/sections/content-audio.toml"),
            &arena,
        )
        .unwrap();

        let sections = document.to::<SectionsDocument>().unwrap();
        let audio = &sections.sections["content-audio"];
        assert_eq!(
            audio
                .target
                .as_ref()
                .unwrap()
                .components()
                .collect::<Vec<_>>(),
            ["settings", "content", "Audio"]
        );
        assert_eq!(audio.options.len(), 13);
    }
}
