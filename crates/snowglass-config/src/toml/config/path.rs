use std::{cmp::Ordering, iter};

use toml_spanner::{Context, Failed, FromToml, Item};

/// A period-separated configuration target path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPath<'de> {
    path: &'de str,
    periods: Vec<usize>,
}

impl<'de> TargetPath<'de> {
    pub(crate) fn from_key(path: &'de str) -> Self {
        Self {
            path,
            periods: path.match_indices('.').map(|(index, _)| index).collect(),
        }
    }

    /// The full path as a string.
    pub fn as_str(&self) -> &'de str {
        self.path
    }

    /// Iterates through the path components.
    pub fn components(&self) -> impl Iterator<Item = &'de str> + '_ {
        self.periods
            .iter()
            .copied()
            .chain(iter::once(self.path.len()))
            .scan(0, |start, end| {
                let component = &self.path[*start..end];
                *start = end + 1;
                Some(component)
            })
    }

    /// Whether `self` is a strict parent of `other`.
    pub fn is_parent_of(&self, other: &Self) -> bool {
        self.periods.len() < other.periods.len()
            && other.path.starts_with(self.path)
            && other.path.as_bytes().get(self.path.len()) == Some(&b'.')
    }

    /// Whether `self` is a strict child of `other`.
    pub fn is_child_of(&self, other: &Self) -> bool {
        other.is_parent_of(self)
    }
}

impl PartialOrd for TargetPath<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            Some(Ordering::Equal)
        } else if self.is_parent_of(other) {
            Some(Ordering::Less)
        } else if self.is_child_of(other) {
            Some(Ordering::Greater)
        } else {
            None
        }
    }
}

impl<'de> FromToml<'de> for TargetPath<'de> {
    fn from_toml(ctx: &mut Context<'de>, item: &Item<'de>) -> Result<Self, Failed> {
        let path = item.require_string(ctx)?;

        if path.is_empty() || path.starts_with('.') || path.ends_with('.') || path.contains("..") {
            return Err(
                ctx.report_custom_error("a target path cannot contain an empty component", item)
            );
        }

        Ok(Self::from_key(path))
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::TargetPath;

    #[test]
    fn checks_target_path_ancestry() {
        let root = TargetPath::from_key("profile-1");
        let controller = TargetPath::from_key("profile-1.emulated_controller");
        let mapping = TargetPath::from_key("profile-1.emulated_controller.mappings");

        assert!(root.is_parent_of(&controller));
        assert!(mapping.is_child_of(&controller));
        assert!(!controller.is_parent_of(&controller));
        assert!(root.is_parent_of(&mapping));
    }

    #[test]
    fn orders_only_related_paths() {
        let root = TargetPath::from_key("profile-1");
        let controller = TargetPath::from_key("profile-1.emulated_controller");
        let sibling = TargetPath::from_key("profile-2");

        assert_eq!(root.partial_cmp(&controller), Some(Ordering::Less));
        assert_eq!(controller.partial_cmp(&root), Some(Ordering::Greater));
        assert_eq!(root.partial_cmp(&root), Some(Ordering::Equal));
        assert_eq!(root.partial_cmp(&sibling), None);
    }
}
