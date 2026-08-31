use std::collections::BTreeMap;

use toml_spanner::{Arena, FromToml, Table, Toml};

use crate::{
    error::Error,
    toml::{
        actions::Actions,
        config::{BoolStyle, Section, Target},
        input::{InputBlock, PortTable},
    },
};

#[derive(Toml)]
#[toml(FromToml)]
pub struct ConfigurationToml<'de> {
    #[toml(default)]
    pub meta: BTreeMap<&'de str, &'de str>,
    #[toml(default)]
    #[toml(rename = "bool_style")]
    pub bool_styles: BTreeMap<&'de str, BoolStyle<'de>>,
    #[toml(default)]
    pub targets: BTreeMap<&'de str, Target<'de>>,
    #[toml(default)]
    pub sections: BTreeMap<&'de str, Section<'de>>,
    pub actions: Option<Actions<'de>>,
    pub input: Option<InputBlock<'de>>,
    #[toml(default)]
    pub platforms: BTreeMap<&'de str, PortTable<'de>>,
}

impl<'de> ConfigurationToml<'de> {
    pub fn try_from_tomls(
        arena: &'de Arena,
        tomls: impl Iterator<Item = &'de str>,
    ) -> Result<Self, crate::error::Error> {
        let mut merged = Table::new();

        for definition in tomls {
            let definition = toml_spanner::parse(definition, arena)?.into_table();
            let mut stack: Vec<(Vec<&'de str>, Table<'de>)> = vec![(Vec::new(), definition)];

            while let Some((path, source)) = stack.pop() {
                let mut target = &mut merged;
                for key in &path {
                    target = target
                        .get_mut(key)
                        .and_then(|item| item.as_table_mut())
                        .ok_or(Error::schema("merge stack paths must identify tables"))?;
                }

                for (key, source_value) in source {
                    if source_value.as_table().is_some() {
                        let source_table = source_value
                            .into_table()
                            .ok_or(Error::schema("could not parse table"))?;

                        if target
                            .get_mut(key.as_str())
                            .and_then(|item| item.as_table_mut())
                            .is_some()
                        {
                            let mut child_path = path.clone();
                            child_path.push(key.as_str());
                            stack.push((child_path, source_table));
                        } else {
                            target.insert(key, source_table.into_item(), arena);
                        }
                    } else {
                        target.insert(key, source_value, arena);
                    }
                }
            }
        }

        // Only way to get an empty context.
        let mut context_document =
            toml_spanner::parse("", arena).expect("Empty document always parses");
        let (context, _) = context_document.split();

        let parsed = Self::from_toml(&mut *context, merged.as_item());

        // `TableHelper::optional` reports a conversion failure to the context
        // and then hands back `None`, and `Vec` does the same per element, so a
        // malformed optional field or array entry produces an `Ok` value with
        // the field silently absent. Anything the context recorded is a schema
        // violation whatever `from_toml` returned — this is the check
        // `Document::to` performs, which driving `from_toml` directly skips.
        let errors = std::mem::take(&mut context.errors);
        if !errors.is_empty() {
            return Err(Error::DefinitionError(errors.into()));
        }

        parsed.map_err(|_| Error::schema("the merged toml failed to parse"))
    }
}

#[cfg(test)]
mod tests {
    use toml_spanner::Arena;

    use super::ConfigurationToml;

    #[test]
    fn merges_definitions_recursively_with_later_values_winning() {
        let arena = Arena::new();
        let configuration = ConfigurationToml::try_from_tomls(
            &arena,
            [
                r#"
                    [sections.gfx]
                    title = "Graphics"

                    [sections.gfx.enhancements]
                    title = "Enhancements"
                    target = "gfx.Enhancements"
                "#,
                r#"
                    [sections.gfx]
                    title = "Video"

                    [sections.gfx.hacks]
                    title = "Hacks"
                    target = "gfx.Hacks"
                "#,
            ]
            .into_iter(),
        )
        .expect("configuration to parse");

        let gfx = &configuration.sections["gfx"];
        assert_eq!(gfx.title, "Video");
        assert!(gfx.sections.contains_key("enhancements"));
        assert!(gfx.sections.contains_key("hacks"));
    }

    /// A section stub carrying no `title` is legal on its own, because the
    /// document that supplies the title may be merged in later.
    #[test]
    fn accepts_a_title_supplied_by_a_later_document() {
        let arena = Arena::new();
        let configuration = ConfigurationToml::try_from_tomls(
            &arena,
            [
                r#"
                    [sections.gfx.hardware]
                    title = "Hardware"
                    target = "gfx.Hardware"
                "#,
                r#"
                    [sections.gfx]
                    title = "Graphics"
                "#,
            ]
            .into_iter(),
        )
        .expect("configuration to parse");

        assert_eq!(configuration.sections["gfx"].title, "Graphics");
    }

    fn reject(definition: &str) -> crate::error::Error {
        let arena = Arena::new();
        ConfigurationToml::try_from_tomls(&arena, [definition].into_iter())
            .err()
            .expect("definition to be rejected")
    }

    /// Every one of these parsed to `Ok` before, with the malformed field
    /// silently absent — `TableHelper::optional` records the failure and hands
    /// back `None`. They are grouped because they share that one cause.
    #[test]
    fn rejects_malformed_tables_rather_than_dropping_them() {
        // A `domain` table matching neither `OptionDomain` variant.
        let error = reject(
            r#"
                [sections.s]
                title = "S"

                [[sections.s.options]]
                key = "o"
                title = "O"
                type = "select"
                domain = { nonsense = 3 }
                emit = { key = "K" }
            "#,
        );
        assert!(!error.diagnostics().is_empty(), "{error}");

        // A `range` whose bounds are not integers.
        let error = reject(
            r#"
                [sections.s]
                title = "S"

                [[sections.s.options]]
                key = "o"
                title = "O"
                type = "int"
                range = { min = "not an int", max = 5 }
                emit = { key = "K" }
            "#,
        );
        assert!(!error.diagnostics().is_empty(), "{error}");

        // A typo inside `emit` used to drop the whole option from the array.
        let error = reject(
            r#"
                [sections.s]
                title = "S"

                [[sections.s.options]]
                key = "o"
                title = "O"
                type = "int"
                emit = { keyy = "K" }
            "#,
        );
        assert!(!error.diagnostics().is_empty(), "{error}");

        // A section missing its required `title`, with nothing to merge it in,
        // used to empty the entire `sections` map.
        let error = reject(
            r#"
                [sections.s]

                [[sections.s.options]]
                key = "o"
                title = "O"
                type = "int"
                emit = { key = "K" }
            "#,
        );
        assert!(!error.diagnostics().is_empty(), "{error}");
    }

    /// A port declaration is `platforms.<machine>.ports.<group>.<index>`, and
    /// `ports` is a field rather than another map level, so a typo there is
    /// caught where it is written rather than parsing as an input group named
    /// after the typo.
    #[test]
    fn rejects_a_misspelled_ports_key() {
        let error = reject(
            r#"
                [platforms.NINTENDO_GCN.porst.controller.0]
                target = "gcpad.GCPad1"
                layouts = ["GAMECUBE_CONTROLLER"]
                emit.unplugged = [{ key = "SIDevice0", value = "0" }]
            "#,
        );
        assert!(!error.diagnostics().is_empty(), "{error}");
    }

    #[test]
    fn parses_a_port_declaration() {
        let arena = Arena::new();
        let configuration = ConfigurationToml::try_from_tomls(
            &arena,
            [r#"
                [platforms.NINTENDO_WII.ports.wii.0]
                target = "wiipad.Wiimote1"
                layouts = ["WII_REMOTE_CONTROLLER"]
                emit.unplugged = [{ key = "Source", value = "0" }]
                emit.passthrough = [{ key = "Source", value = "2" }]
                emit.WII_REMOTE_CONTROLLER = [{ key = "Source", value = "1" }]
            "#]
            .into_iter(),
        )
        .expect("configuration to parse");

        let port = &configuration.platforms["NINTENDO_WII"].ports["wii"]["0"];
        assert_eq!(port.layouts, ["WII_REMOTE_CONTROLLER"]);

        // Passthrough support is declared by emitting for it, nothing else.
        assert!(port.emit.contains_key("passthrough"));
    }

    /// One pass reports every violation, not just the first.
    #[test]
    fn reports_every_violation_in_one_pass() {
        let error = reject(
            r#"
                [sections.a]
                title = "A"

                [[sections.a.options]]
                key = "one"
                title = "One"
                type = "int"
                range = { min = "bad", max = 5 }
                emit = { key = "K" }

                [[sections.a.options]]
                key = "two"
                title = "Two"
                type = "select"
                domain = { nonsense = 3 }
                emit = { key = "L" }
            "#,
        );

        assert!(
            error.diagnostics().len() >= 2,
            "expected both violations, got {:?}",
            error.diagnostics()
        );
    }

    /// Both spellings of a domain survive the round trip. `OptionDomain` needs
    /// `untagged`; without it neither variant matches and `Option<_>` turns
    /// that into a silent `None`.
    #[test]
    fn parses_both_domain_forms() {
        use crate::toml::config::OptionDomain;

        let arena = Arena::new();
        let configuration = ConfigurationToml::try_from_tomls(
            &arena,
            [r#"
                [sections.s]
                title = "S"

                [[sections.s.options]]
                key = "static"
                title = "Static"
                type = "select"
                domain = [{ key = "A", label = "A", value = "0" }]
                emit = { key = "K" }

                [[sections.s.options]]
                key = "resolved"
                title = "Resolved"
                type = "select"
                domain = { source = "resolver:x" }
                emit = { key = "L" }
            "#]
            .into_iter(),
        )
        .expect("configuration to parse");

        let options = &configuration.sections["s"].options;
        assert!(matches!(
            options[0].domain,
            Some(OptionDomain::Select(ref choices)) if choices.len() == 1
        ));
        assert!(matches!(
            options[1].domain,
            Some(OptionDomain::Resolve {
                source: "resolver:x"
            })
        ));
    }
}

/// Parses every plugin package in the repository exactly as a host would:
/// expand the manifest's `definitions` list and merge the result into one
/// document.
///
/// Each bug this has caught was silent before [`Error::DefinitionError`]
/// existed — a whole block failing to match a schema looks exactly like a block
/// nobody wrote.
#[cfg(test)]
mod packages {
    use std::{collections::BTreeMap, fs, path::Path};

    use toml_spanner::{Arena, Toml};

    use super::ConfigurationToml;
    use crate::toml::config::Target;

    // The manifest is the host's to parse, not this crate's (§2). Modelled
    // here only far enough to reach `definitions`, and completely enough that
    // the schema does not reject the keys it ignores.
    #[derive(Toml)]
    #[toml(FromToml)]
    struct Manifest<'de> {
        #[expect(dead_code)]
        plugin: ManifestPlugin<'de>,
        emulator: ManifestEmulator<'de>,
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct ManifestPlugin<'de> {
        #[expect(dead_code)]
        id: &'de str,
        #[expect(dead_code)]
        version: &'de str,
        #[expect(dead_code)]
        class: &'de str,
        #[toml(default)]
        #[expect(dead_code)]
        config: BTreeMap<&'de str, &'de str>,
        #[toml(default)]
        #[expect(dead_code)]
        resources: Vec<ManifestResource<'de>>,
        #[toml(default)]
        #[expect(dead_code)]
        caches: Vec<ManifestCache<'de>>,
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct ManifestResource<'de> {
        #[expect(dead_code)]
        name: &'de str,
        #[expect(dead_code)]
        target: Option<&'de str>,
        #[expect(dead_code)]
        source: &'de str,
        #[expect(dead_code)]
        format: &'de str,
        #[expect(dead_code)]
        mode: &'de str,
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct ManifestCache<'de> {
        #[expect(dead_code)]
        key: &'de str,
        #[expect(dead_code)]
        label: &'de str,
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct ManifestEmulator<'de> {
        #[expect(dead_code)]
        platforms: Vec<&'de str>,
        #[expect(dead_code)]
        main: Option<&'de str>,
        configuration: ManifestConfiguration<'de>,
    }

    #[derive(Toml)]
    #[toml(FromToml)]
    struct ManifestConfiguration<'de> {
        definitions: Vec<&'de str>,
        #[expect(dead_code)]
        resolver: Option<&'de str>,
    }

    /// Expands one `definitions` entry, which is either a literal path or a
    /// single `*` wildcard in the final component.
    fn expand(root: &Path, pattern: &str) -> Vec<std::path::PathBuf> {
        let Some((head, tail)) = pattern.split_once('*') else {
            return vec![root.join(pattern)];
        };
        let (dir, prefix) = head.rsplit_once('/').unwrap_or(("", head));

        let mut matched: Vec<_> = fs::read_dir(root.join(dir))
            .unwrap_or_else(|error| panic!("reading {dir}: {error}"))
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                path.is_file() && name.starts_with(prefix) && name.ends_with(tail)
            })
            .collect();
        matched.sort();
        matched
    }

    fn definitions(root: &Path) -> Vec<std::path::PathBuf> {
        let arena = Arena::new();
        let manifest = fs::read_to_string(root.join("plugin.toml")).expect("a manifest");
        let mut manifest = toml_spanner::parse(&manifest, &arena).expect("manifest to parse");
        let manifest = manifest.to::<Manifest>().expect("manifest to match");

        manifest
            .emulator
            .configuration
            .definitions
            .iter()
            .flat_map(|pattern| expand(root, pattern))
            .collect()
    }

    #[test]
    fn every_plugin_package_parses() {
        let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");

        let mut checked = 0;
        for entry in fs::read_dir(&plugins).expect("a plugins directory") {
            let root = entry.unwrap().path();
            if !root.join("plugin.toml").is_file() {
                continue;
            }
            let name = root.file_name().unwrap().to_string_lossy().into_owned();

            let files = definitions(&root);
            assert!(!files.is_empty(), "{name} declares no definitions");

            // Held across the arena's lifetime so the borrowed document can
            // outlive the parse.
            let sources: Vec<String> = files
                .iter()
                .map(|file| {
                    fs::read_to_string(file)
                        .unwrap_or_else(|error| panic!("{}: {error}", file.display()))
                })
                .collect();

            let arena = Arena::new();
            let configuration =
                ConfigurationToml::try_from_tomls(&arena, sources.iter().map(String::as_str))
                    .unwrap_or_else(|error| {
                        let details: Vec<_> = error
                            .diagnostics()
                            .iter()
                            .map(|diagnostic| format!("{diagnostic:?}"))
                            .collect();
                        panic!("{name}: {error}\n{}", details.join("\n"))
                    });

            // A definition that parses to nothing is the failure mode this
            // whole test exists to catch, so assert on substance rather than
            // on `Ok`.
            assert!(!configuration.sections.is_empty(), "{name}: no sections");
            assert!(!configuration.targets.is_empty(), "{name}: no targets");

            // Walk the target tree accumulating each node's declaration path.
            // The collector this replaced recorded only leaves, and recorded
            // them under their bare last component, so both the depth and the
            // dotted ids below are the regression guard.
            let mut nodes: Vec<(String, bool)> = Vec::new();
            let mut stack: Vec<(String, &Target)> = configuration
                .targets
                .iter()
                .map(|(key, target)| ((*key).to_owned(), target))
                .collect();
            while let Some((id, target)) = stack.pop() {
                nodes.push((id.clone(), target.output.is_some()));
                for (key, child) in &target.children {
                    stack.push((format!("{id}.{key}"), child));
                }
            }

            assert!(
                nodes.iter().any(|(_, file)| *file),
                "{name}: no target declares a file"
            );
            assert!(
                nodes.iter().any(|(id, _)| id.contains('.')),
                "{name}: no target below a root — only leaves were collected"
            );
            assert!(
                !configuration.bool_styles.is_empty(),
                "{name}: no bool styles"
            );

            for (id, style) in &configuration.bool_styles {
                assert!(
                    !style.r#true.is_empty() && !style.r#false.is_empty(),
                    "{name}: bool style {id} has an empty member"
                );
            }

            let input = configuration
                .input
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: no input block"));
            assert!(!input.mapping.mapping.is_empty(), "{name}: no sdl mapping");
            assert!(!input.layouts.is_empty(), "{name}: no layouts");

            let ports: usize = configuration
                .platforms
                .values()
                .flat_map(|table| table.ports.values())
                .map(BTreeMap::len)
                .sum();
            assert!(ports > 0, "{name}: no ports");

            // Every choice a port emits for is `unplugged`, `passthrough`, or a
            // layout the port offers — there is no separate field declaring
            // passthrough support.
            for table in configuration.platforms.values() {
                for group in table.ports.values() {
                    for (index, port) in group {
                        for choice in port.emit.keys() {
                            assert!(
                                *choice == "unplugged"
                                    || *choice == "passthrough"
                                    || port.layouts.contains(choice),
                                "{name}: port {index} emits for unknown choice {choice}"
                            );
                        }
                    }
                }
            }

            let owned = crate::ast::ConfigurationDefinition::from(configuration);
            assert!(!owned.sections.is_empty(), "{name}: no owned sections");
            assert!(!owned.targets.is_empty(), "{name}: no owned targets");
            assert!(!owned.options.is_empty(), "{name}: no owned options");

            checked += 1;
        }

        assert!(checked >= 4, "expected every package, checked {checked}");
    }
}
