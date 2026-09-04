bitflags::bitflags! {
    /// Access a NAND grants for a path, in the same bit layout as
    /// `polymer_vfs::Permissions` so a mount can pass it straight through.
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    pub struct Permissions: u8 {
        const READ = 1;
        const WRITE = 2;
        const EXECUTE = 4;

        const READ_EXECUTE = Self::READ.bits() | Self::EXECUTE.bits();
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PermissionsTable(&'static [(&'static str, Permissions)]);

impl Permissions {
    pub const fn can_read(self) -> bool {
        self.contains(Self::READ)
    }

    pub const fn can_write(self) -> bool {
        self.contains(Self::WRITE)
    }

    pub const fn can_execute(self) -> bool {
        self.contains(Self::EXECUTE)
    }
}

impl std::fmt::Display for Permissions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (allowed, letter) in [
            (self.can_read(), "r"),
            (self.can_write(), "w"),
            (self.can_execute(), "x"),
        ] {
            if allowed {
                formatter.write_str(letter)?;
            }
        }

        Ok(())
    }
}

impl PermissionsTable {
    pub const fn new(entries: &'static [(&'static str, Permissions)]) -> Self {
        Self(entries)
    }

    /// What is granted for `path`. Anything no prefix covers is denied.
    pub fn get(&self, path: &str) -> Permissions {
        self.0
            .iter()
            .filter(|(prefix, _)| {
                prefix.is_empty()
                    || path == *prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|(prefix, _)| prefix.len())
            .map_or(Permissions::empty(), |(_, permissions)| *permissions)
    }

    /// The entries as declared, for building mount bindings.
    pub const fn entries(&self) -> &'static [(&'static str, Permissions)] {
        self.0
    }
}
