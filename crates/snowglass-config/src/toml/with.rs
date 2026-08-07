use toml_spanner::{Context, Failed, Item, Key, Table};

pub(super) mod target_paths {
    use crate::toml::config::TargetPath;

    use super::*;

    pub fn init<'de>() -> Vec<TargetPath<'de>> {
        Vec::new()
    }

    pub fn insert<'de>(
        _ctx: &mut Context<'de>,
        key: &Key<'de>,
        item: &Item<'de>,
        paths: &mut Vec<TargetPath<'de>>,
    ) -> Result<(), Failed> {
        collect(key.as_str(), item, paths);
        Ok(())
    }

    fn collect<'de>(path: &'de str, item: &Item<'de>, paths: &mut Vec<TargetPath<'de>>) {
        let Some(table) = item.as_table() else {
            return;
        };

        if table.entries().is_empty() {
            paths.push(TargetPath::from_key(path));
            return;
        }

        for (key, item) in table.entries() {
            collect(key.as_str(), item, paths);
        }
    }

    pub fn finish<'de>(
        _ctx: &mut Context<'de>,
        _parent: &Table<'de>,
        paths: Vec<TargetPath<'de>>,
    ) -> Result<Vec<TargetPath<'de>>, Failed> {
        Ok(paths)
    }
}

pub(super) mod binding_map {
    use consolespec::BindingKey;
    use indexmap::IndexMap;

    use super::*;

    pub fn init<'de>() -> IndexMap<BindingKey, &'de str> {
        IndexMap::new()
    }

    pub fn insert<'de>(
        ctx: &mut Context<'de>,
        key: &Key<'de>,
        item: &Item<'de>,
        mappings: &mut IndexMap<BindingKey, &'de str>,
    ) -> Result<(), Failed> {
        let binding = key
            .as_str()
            .parse()
            .map_err(|error| ctx.report_custom_error(error, item))?;
        let value = item.require_string(ctx)?;
        mappings.insert(binding, value);
        Ok(())
    }

    pub fn finish<'de>(
        _ctx: &mut Context<'de>,
        _parent: &Table<'de>,
        mappings: IndexMap<BindingKey, &'de str>,
    ) -> Result<IndexMap<BindingKey, &'de str>, Failed> {
        Ok(mappings)
    }
}
