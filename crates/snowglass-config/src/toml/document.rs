use std::collections::BTreeMap;

use toml_spanner::{Arena, FromToml, Table, Toml};

use crate::{
    error::Error,
    toml::{
        config::{BoolStyle, Section, Target},
        input::InputBlock,
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
    pub input: Option<InputBlock<'de>>,
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
                        let source_table = source_value.into_table()
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
        Self::from_toml(context, merged.as_item())
            .map_err(|_| Error::schema("the merged toml failed to parse"))
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
}
