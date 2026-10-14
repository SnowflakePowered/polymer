use crate::PackageError;

/// A plugin ID: a bare name for official plugins, or `@scope/name` for every other publisher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginId<'a> {
    pub scope: Option<&'a str>,
    pub name: &'a str,
}

impl<'a> PluginId<'a> {
    pub fn parse(id: &'a str) -> Result<Self, PackageError> {
        let (scope, name) = match id.strip_prefix('@') {
            Some(scoped) => scoped
                .split_once('/')
                .map(|(scope, name)| (Some(scope), name))
                .ok_or_else(|| PackageError::InvalidPluginId(id.to_owned()))?,
            None => (None, id),
        };
        if !Self::is_valid_name(name) || scope.is_some_and(|scope| !Self::is_valid_name(scope)) {
            return Err(PackageError::InvalidPluginId(id.to_owned()));
        }
        Ok(Self { scope, name })
    }

    /// Whether `part` can be a scope or a plugin name: lowercase ASCII letters, digits, `-` and
    /// `_`.
    pub fn is_valid_name(part: &str) -> bool {
        !part.is_empty()
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
    }
}
