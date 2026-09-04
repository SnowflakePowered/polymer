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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use toml_spanner::{Arena, Toml};

    use super::{Section, Target, TargetPath};

    #[derive(Toml)]
    #[toml(FromToml)]
    struct Document<'de> {
        target: TargetPath<'de>,
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct TargetsDocument<'de> {
        targets: BTreeMap<&'de str, Target<'de>>,
    }

    fn targets<'a>(arena: &'a Arena, source: &'a str) -> Result<TargetsDocument<'a>, String> {
        let mut document = toml_spanner::parse(source, arena).expect("well-formed toml");
        document.to::<TargetsDocument>().map_err(|e| e.to_string())
    }

    /// The declaration path is the id and nesting is the structure, so a target
    /// tree is walked rather than collected — which is what makes an interior
    /// node with both options and children representable.
    #[test]
    fn parses_nested_targets_to_full_depth() {
        let arena = Arena::new();
        let parsed = targets(
            &arena,
            r#"
                [targets.settings]
                output = { format = "xml", path = "settings.xml" }

                [targets.settings.content]
                [targets.settings.content.Graphic]
                [targets.settings.content.Graphic.Overlay]
            "#,
        )
        .expect("targets to parse");

        let settings = &parsed.targets["settings"];
        let output = settings.output.as_ref().expect("a file");
        assert_eq!((output.format, output.path), ("xml", "settings.xml"));

        let content = &settings.children["content"];
        assert!(content.output.is_none());

        // The interior node is itself a target, not merely a step on the way to
        // a leaf: it has a child *and* is addressable in its own right.
        let graphic = &content.children["Graphic"];
        assert!(graphic.children.contains_key("Overlay"));
    }

    /// `parent` is the escape hatch for a subtree reachable from more than one
    /// root. The declaration path is still the emitted name.
    #[test]
    fn parses_parent() {
        let arena = Arena::new();
        let parsed = targets(
            &arena,
            r#"
                [targets.retroarch-core]
                output = { format = "cfg", path = "cores/core.cfg" }

                [targets.retroarch-global]
                output = { format = "cfg", path = "retroarch.cfg" }

                [targets.video]
                parent = ["retroarch-core", "retroarch-global"]

                [targets.video.scaling]
            "#,
        )
        .expect("targets to parse");

        // A root in its own right once attached, but not a file of its own.
        let shared = &parsed.targets["video"];
        assert!(shared.output.is_none());

        let parents: Vec<_> = shared.parent.iter().map(TargetPath::as_str).collect();
        assert_eq!(parents, ["retroarch-core", "retroarch-global"]);

        // `parent` is a declared field, so it is consumed before the flatten
        // drain and never becomes a child.
        assert_eq!(
            shared.children.keys().copied().collect::<Vec<_>>(),
            ["scaling"]
        );
    }

    /// The collector this replaced skipped any non-table value, which would
    /// have silently swallowed a misspelled `parent`.
    #[test]
    fn rejects_a_scalar_where_a_child_target_belongs() {
        let arena = Arena::new();
        let error = targets(
            &arena,
            r#"
                [targets.gfx]
                output = { format = "ini", path = "GFX.ini" }
                nonsense = 3
            "#,
        )
        .err()
        .expect("a scalar child to be rejected");

        assert!(error.contains("targets.gfx.nonsense"), "{error}");
    }

    /// A format without a path is now unspellable rather than a lowering check:
    /// `output` is one table, and half of it fails to convert.
    #[test]
    fn rejects_an_output_missing_its_path() {
        let arena = Arena::new();
        let error = targets(
            &arena,
            r#"
                [targets.gfx]
                output = { format = "ini" }
            "#,
        )
        .err()
        .expect("a half-declared output to be rejected");

        assert!(error.contains("path"), "{error}");
    }

    /// A root with no `output` is realized but never written.
    #[test]
    fn accepts_a_root_with_no_output() {
        let arena = Arena::new();
        let parsed = targets(
            &arena,
            r#"
                [targets.install]
                [targets.install.paths]
            "#,
        )
        .expect("targets to parse");

        let install = &parsed.targets["install"];
        assert!(install.output.is_none());
        assert!(install.children.contains_key("paths"));
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
            include_str!("../../../../../plugins/cemu/config/sections/content-audio.toml"),
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
