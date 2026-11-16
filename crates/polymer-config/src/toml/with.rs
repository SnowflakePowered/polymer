use toml_spanner::{Context, Failed, Item, Key, Table};

pub(super) mod binding_map {
    use consolespec::input::BindingKey;
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

pub(super) mod ordered_strings {
    use indexmap::IndexMap;

    use super::*;

    pub fn init<'de>() -> IndexMap<&'de str, &'de str> {
        IndexMap::new()
    }

    pub fn insert<'de>(
        ctx: &mut Context<'de>,
        key: &Key<'de>,
        item: &Item<'de>,
        values: &mut IndexMap<&'de str, &'de str>,
    ) -> Result<(), Failed> {
        values.insert(key.as_str(), item.require_string(ctx)?);
        Ok(())
    }

    pub fn finish<'de>(
        _ctx: &mut Context<'de>,
        _parent: &Table<'de>,
        values: IndexMap<&'de str, &'de str>,
    ) -> Result<IndexMap<&'de str, &'de str>, Failed> {
        Ok(values)
    }
}

pub(super) mod binding_value_map {
    use consolespec::input::BindingKey;
    use indexmap::IndexMap;

    use super::*;
    use crate::toml::input::MappingValue;

    pub fn init<'de>() -> IndexMap<BindingKey, MappingValue<'de>> {
        IndexMap::new()
    }

    pub fn insert<'de>(
        ctx: &mut Context<'de>,
        key: &Key<'de>,
        item: &Item<'de>,
        mappings: &mut IndexMap<BindingKey, MappingValue<'de>>,
    ) -> Result<(), Failed> {
        let binding = key
            .as_str()
            .parse()
            .map_err(|error| ctx.report_custom_error(error, item))?;
        let value = if let Some(value) = item.as_i64() {
            MappingValue::Integer(value)
        } else if let Some(array) = item.as_array() {
            MappingValue::List(
                array
                    .iter()
                    .map(|element| element.require_string(ctx))
                    .collect::<Result<_, _>>()?,
            )
        } else {
            MappingValue::Text(item.require_string(ctx)?)
        };
        mappings.insert(binding, value);
        Ok(())
    }

    pub fn finish<'de>(
        _ctx: &mut Context<'de>,
        _parent: &Table<'de>,
        mappings: IndexMap<BindingKey, MappingValue<'de>>,
    ) -> Result<IndexMap<BindingKey, MappingValue<'de>>, Failed> {
        Ok(mappings)
    }
}
