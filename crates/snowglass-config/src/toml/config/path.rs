use toml_spanner::{Context, Failed, FromToml, Item};

pub use crate::ast::TargetPath;
use crate::ast::{ConfigurationPath, family};

impl<'de> FromToml<'de> for ConfigurationPath<&'de str, family::Target> {
    fn from_toml(ctx: &mut Context<'de>, item: &Item<'de>) -> Result<Self, Failed> {
        let path = item.require_string(ctx)?;

        if !Self::is_valid(path) {
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
