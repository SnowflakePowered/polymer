//! Loading helpers for plugin configuration documents.
//!
//! A definition may move a section to another TOML file by placing an
//! `import` string in an otherwise empty section. The imported file is still a
//! rooted definition document: an import at `sections.video` extracts the
//! `sections.video` subtree from that document and splices it at the import's
//! position.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use toml::{Table, Value};

/// Loads a definition and expands its section imports.
///
/// `definition` is relative to `package_root`. Import paths are relative to the
/// file containing the import and must remain inside `package_root`.
///
/// An imported document must contain only the section named by its splice
/// point. For example, `[sections.video] import = "video.toml"` expects
/// `video.toml` to contain `[sections.video]`, `[[sections.video.options]]`, or
/// both. The importing section may contain no field other than `import`.
pub fn load_definition(
    package_root: impl AsRef<Path>,
    definition: impl AsRef<Path>,
) -> Result<Value, LoadError> {
    let package_root =
        fs::canonicalize(package_root.as_ref()).map_err(|source| LoadError::Read {
            path: package_root.as_ref().to_path_buf(),
            source,
        })?;
    let definition = resolve_path(&package_root, &package_root, definition.as_ref())?;
    let mut stack = vec![definition.clone()];
    let mut document = parse_document(&definition)?;
    resolve_document(&package_root, &definition, &mut document, &mut stack)?;
    Ok(document)
}

fn parse_document(path: &Path) -> Result<Value, LoadError> {
    let source = fs::read_to_string(path).map_err(|source| LoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&source).map_err(|source| LoadError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn resolve_document(
    package_root: &Path,
    source_path: &Path,
    document: &mut Value,
    stack: &mut Vec<PathBuf>,
) -> Result<(), LoadError> {
    let Some(sections) = document.get_mut("sections") else {
        return Ok(());
    };
    let sections = sections
        .as_table_mut()
        .ok_or_else(|| LoadError::InvalidSections {
            path: source_path.to_path_buf(),
        })?;

    for (name, section) in sections.iter_mut() {
        let section = section
            .as_table_mut()
            .ok_or_else(|| LoadError::InvalidSection {
                path: source_path.to_path_buf(),
                section: format!("sections.{name}"),
            })?;
        let mut section_path = vec![name.clone()];
        resolve_section(package_root, source_path, &mut section_path, section, stack)?;
    }
    Ok(())
}

fn resolve_section(
    package_root: &Path,
    source_path: &Path,
    section_path: &mut Vec<String>,
    section: &mut Table,
    stack: &mut Vec<PathBuf>,
) -> Result<(), LoadError> {
    if let Some(import) = section.get("import") {
        let import = import.as_str().ok_or_else(|| LoadError::InvalidImport {
            path: source_path.to_path_buf(),
            section: display_section(section_path),
        })?;
        if section.len() != 1 {
            return Err(LoadError::ImportWithBody {
                path: source_path.to_path_buf(),
                section: display_section(section_path),
            });
        }

        let base = source_path.parent().unwrap_or(package_root);
        let imported_path = resolve_path(package_root, base, Path::new(import))?;
        if let Some(cycle_start) = stack.iter().position(|path| path == &imported_path) {
            let mut chain = stack[cycle_start..].to_vec();
            chain.push(imported_path);
            return Err(LoadError::Cycle { chain });
        }

        stack.push(imported_path.clone());
        let result = (|| {
            let document = parse_document(&imported_path)?;
            let mut imported = take_exact_section(document, section_path, &imported_path)?;
            resolve_section(
                package_root,
                &imported_path,
                section_path,
                &mut imported,
                stack,
            )?;
            Ok(imported)
        })();
        stack.pop();
        *section = result?;
    }

    // Every table directly under a section is another section. `options` is
    // the sole reserved child and is an array of tables.
    for (name, child) in section.iter_mut() {
        if name == "options" {
            continue;
        }
        if let Some(child) = child.as_table_mut() {
            section_path.push(name.clone());
            let result = resolve_section(package_root, source_path, section_path, child, stack);
            section_path.pop();
            result?;
        }
    }
    Ok(())
}

/// Extracts `sections.<section_path>` and rejects every other root or section.
fn take_exact_section(
    document: Value,
    section_path: &[String],
    source_path: &Path,
) -> Result<Table, LoadError> {
    let expected = display_section(section_path);
    let Value::Table(mut root) = document else {
        return Err(LoadError::ImportedDocumentShape {
            path: source_path.to_path_buf(),
            expected,
        });
    };
    if root.len() != 1 {
        return Err(LoadError::ImportedDocumentShape {
            path: source_path.to_path_buf(),
            expected,
        });
    }
    let Some(Value::Table(mut current)) = root.remove("sections") else {
        return Err(LoadError::ImportedDocumentShape {
            path: source_path.to_path_buf(),
            expected: display_section(section_path),
        });
    };

    for (index, component) in section_path.iter().enumerate() {
        if current.len() != 1 {
            return Err(LoadError::ImportedDocumentShape {
                path: source_path.to_path_buf(),
                expected: display_section(section_path),
            });
        }
        let Some(Value::Table(next)) = current.remove(component) else {
            return Err(LoadError::MissingImportedSection {
                path: source_path.to_path_buf(),
                expected: display_section(section_path),
            });
        };
        if index + 1 == section_path.len() {
            return Ok(next);
        }
        current = next;
    }

    unreachable!("section paths are never empty")
}

fn resolve_path(package_root: &Path, base: &Path, referenced: &Path) -> Result<PathBuf, LoadError> {
    if referenced.is_absolute() {
        return Err(LoadError::OutsidePackage {
            path: referenced.to_path_buf(),
        });
    }
    let unresolved = base.join(referenced);
    let path = fs::canonicalize(&unresolved).map_err(|source| LoadError::Read {
        path: unresolved,
        source,
    })?;
    if !path.starts_with(package_root) {
        return Err(LoadError::OutsidePackage { path });
    }
    Ok(path)
}

fn display_section(path: &[String]) -> String {
    format!("sections.{}", path.join("."))
}

/// Failure while reading or expanding a definition document.
#[derive(Debug)]
pub enum LoadError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    InvalidSections {
        path: PathBuf,
    },
    InvalidSection {
        path: PathBuf,
        section: String,
    },
    InvalidImport {
        path: PathBuf,
        section: String,
    },
    ImportWithBody {
        path: PathBuf,
        section: String,
    },
    OutsidePackage {
        path: PathBuf,
    },
    MissingImportedSection {
        path: PathBuf,
        expected: String,
    },
    ImportedDocumentShape {
        path: PathBuf,
        expected: String,
    },
    Cycle {
        chain: Vec<PathBuf>,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(formatter, "invalid TOML in {}: {source}", path.display())
            }
            Self::InvalidSections { path } => {
                write!(
                    formatter,
                    "`sections` in {} must be a table",
                    path.display()
                )
            }
            Self::InvalidSection { path, section } => {
                write!(formatter, "{section} in {} must be a table", path.display())
            }
            Self::InvalidImport { path, section } => write!(
                formatter,
                "{section}.import in {} must be a string",
                path.display()
            ),
            Self::ImportWithBody { path, section } => write!(
                formatter,
                "{section} in {} may contain only `import` when importing",
                path.display()
            ),
            Self::OutsidePackage { path } => {
                write!(
                    formatter,
                    "import escapes the plugin package: {}",
                    path.display()
                )
            }
            Self::MissingImportedSection { path, expected } => write!(
                formatter,
                "{} does not define the imported section {expected}",
                path.display()
            ),
            Self::ImportedDocumentShape { path, expected } => write!(
                formatter,
                "{} must contain only the imported section {expected}",
                path.display()
            ),
            Self::Cycle { chain } => {
                formatter.write_str("cyclic section import: ")?;
                for (index, path) in chain.iter().enumerate() {
                    if index != 0 {
                        formatter.write_str(" -> ")?;
                    }
                    write!(formatter, "{}", path.display())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LoadError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        for (path, source) in files {
            let path = directory.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        directory
    }

    #[test]
    fn splices_a_rooted_section_relative_to_the_importing_file() {
        let directory = fixture(&[
            (
                "config/main.toml",
                "[sections.before]\ntitle = \"Before\"\n[sections.content]\nimport = \"content.toml\"\n[sections.after]\ntitle = \"After\"\n",
            ),
            (
                "config/content.toml",
                "[sections.content]\ntitle = \"Content\"\ntarget = \"settings.content\"\n[[sections.content.options]]\nkey = \"enabled\"\ntype = \"bool\"\n",
            ),
        ]);

        let document = load_definition(directory.path(), "config/main.toml").unwrap();
        let sections = document["sections"].as_table().unwrap();
        assert_eq!(
            sections.keys().collect::<Vec<_>>(),
            ["before", "content", "after"]
        );
        assert_eq!(sections["content"]["title"].as_str(), Some("Content"));
        assert_eq!(sections["content"]["options"].as_array().unwrap().len(), 1);
        assert!(sections["content"].get("import").is_none());
    }

    #[test]
    fn resolves_nested_imports_using_their_own_file_and_full_section_path() {
        let directory = fixture(&[
            (
                "config/main.toml",
                "[sections.video]\nimport = \"parts/video.toml\"\n",
            ),
            (
                "config/parts/video.toml",
                "[sections.video]\ntitle = \"Video\"\n[sections.video.advanced]\nimport = \"advanced.toml\"\n",
            ),
            (
                "config/parts/advanced.toml",
                "[sections.video.advanced]\ntitle = \"Advanced\"\n[[sections.video.advanced.options]]\nkey = \"debug\"\ntype = \"bool\"\n",
            ),
        ]);

        let document = load_definition(directory.path(), "config/main.toml").unwrap();
        assert_eq!(
            document["sections"]["video"]["advanced"]["title"].as_str(),
            Some("Advanced")
        );
    }

    #[test]
    fn rejects_an_import_with_a_local_body() {
        let directory = fixture(&[
            (
                "config/main.toml",
                "[sections.content]\nimport = \"content.toml\"\ntitle = \"Override\"\n",
            ),
            (
                "config/content.toml",
                "[sections.content]\ntitle = \"Content\"\n",
            ),
        ]);

        assert!(matches!(
            load_definition(directory.path(), "config/main.toml"),
            Err(LoadError::ImportWithBody { .. })
        ));
    }

    #[test]
    fn rejects_unrelated_content_in_an_imported_document() {
        let directory = fixture(&[
            (
                "config/main.toml",
                "[sections.content]\nimport = \"content.toml\"\n",
            ),
            (
                "config/content.toml",
                "[sections.content]\ntitle = \"Content\"\n[sections.unrelated]\ntitle = \"Nope\"\n",
            ),
        ]);

        assert!(matches!(
            load_definition(directory.path(), "config/main.toml"),
            Err(LoadError::ImportedDocumentShape { .. })
        ));
    }

    #[test]
    fn rejects_a_missing_qualified_section() {
        let directory = fixture(&[
            (
                "config/main.toml",
                "[sections.content]\nimport = \"content.toml\"\n",
            ),
            (
                "config/content.toml",
                "[sections.other]\ntitle = \"Other\"\n",
            ),
        ]);

        assert!(matches!(
            load_definition(directory.path(), "config/main.toml"),
            Err(LoadError::MissingImportedSection { .. })
        ));
    }

    #[test]
    fn reports_the_full_cycle() {
        let directory = fixture(&[
            (
                "config/main.toml",
                "[sections.content]\nimport = \"a.toml\"\n",
            ),
            ("config/a.toml", "[sections.content]\nimport = \"b.toml\"\n"),
            ("config/b.toml", "[sections.content]\nimport = \"a.toml\"\n"),
        ]);

        let error = load_definition(directory.path(), "config/main.toml").unwrap_err();
        let LoadError::Cycle { chain } = error else {
            panic!("expected cycle error, got {error}");
        };
        assert_eq!(chain.len(), 3);
        assert!(chain[0].ends_with("a.toml"));
        assert!(chain[1].ends_with("b.toml"));
        assert!(chain[2].ends_with("a.toml"));
    }

    #[test]
    fn rejects_imports_outside_the_package() {
        let parent = tempfile::tempdir().unwrap();
        let package = parent.path().join("plugin");
        fs::create_dir_all(package.join("config")).unwrap();
        fs::write(parent.path().join("outside.toml"), "[sections.content]\n").unwrap();
        fs::write(
            package.join("config/main.toml"),
            "[sections.content]\nimport = \"../../outside.toml\"\n",
        )
        .unwrap();

        assert!(matches!(
            load_definition(&package, "config/main.toml"),
            Err(LoadError::OutsidePackage { .. })
        ));
    }

    #[test]
    fn loads_the_cemu_split_pilot() {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/cemu");
        let document = load_definition(package, "config/cemu.toml").unwrap();
        let sections = document["sections"].as_table().unwrap();

        assert_eq!(
            sections.keys().collect::<Vec<_>>(),
            ["content", "content-debug", "content-graphic"]
        );
        assert_eq!(
            sections
                .values()
                .map(|section| section["options"].as_array().unwrap().len())
                .sum::<usize>(),
            23
        );
        assert!(
            sections
                .values()
                .all(|section| section.get("import").is_none())
        );
    }

    #[test]
    fn loads_the_dolphin_split_definition() {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/dolphin");
        let document = load_definition(package, "config/dolphin.toml").unwrap();
        let sections = document["sections"].as_table().unwrap();

        assert_eq!(sections.keys().collect::<Vec<_>>(), ["gfx", "main"]);
        let children = sections.values().flat_map(|section| {
            section
                .as_table()
                .unwrap()
                .values()
                .filter_map(Value::as_table)
        });
        let (section_count, option_count) = children.fold((0, 0), |counts, section| {
            assert!(section.get("import").is_none());
            (
                counts.0 + 1,
                counts.1 + section["options"].as_array().unwrap().len(),
            )
        });
        assert_eq!((section_count, option_count), (27, 443));
        assert!(
            sections
                .values()
                .all(|section| section.get("import").is_none())
        );
    }

    #[test]
    fn loads_the_pcsx2_split_definition() {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/pcsx2");
        let document = load_definition(package, "config/pcsx2.toml").unwrap();
        let sections = document["sections"].as_table().unwrap();

        assert_eq!(sections.len(), 19);
        assert_eq!(
            sections
                .values()
                .map(|section| section["options"].as_array().unwrap().len())
                .sum::<usize>(),
            358
        );
        assert!(
            sections
                .values()
                .all(|section| section.get("import").is_none())
        );
    }

    #[test]
    fn loads_the_duckstation_split_definition() {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/duckstation");
        let document = load_definition(package, "config/duckstation.toml").unwrap();
        let sections = document["sections"].as_table().unwrap();

        assert_eq!(sections.len(), 17);
        assert_eq!(
            sections
                .values()
                .map(|section| section["options"].as_array().unwrap().len())
                .sum::<usize>(),
            204
        );
        assert!(
            sections
                .values()
                .all(|section| section.get("import").is_none())
        );
    }
}
