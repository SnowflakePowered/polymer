use std::{fmt, str::FromStr};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    pub struct Permissions: u8 {
        const READ = 1;
        const WRITE = 2;
        const EXECUTE = 4;
        /// The backend can adopt a source node as the file at this path.
        const ADOPT = 8;

        const READ_EXECUTE = Self::READ.bits() | Self::EXECUTE.bits();
        const READ_WRITE_EXECUTE = Self::READ.bits() | Self::WRITE.bits() | Self::EXECUTE.bits();
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid permission mode '{0}'")]
pub struct InvalidPermissions(pub String);

#[derive(Debug, Clone, Copy)]
pub struct PermissionsTable(&'static [(&'static str, Permissions)]);

impl Permissions {
    pub const NONE: Self = Self::empty();
    pub const ALL: Self = Self::all();

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

impl FromStr for Permissions {
    type Err = InvalidPermissions;

    fn from_str(value: &str) -> Result<Self, InvalidPermissions> {
        let expected = value
            .bytes()
            .try_fold((0_u8, 0_u8), |(bits, previous), byte| {
                let (order, bit) = match byte {
                    b'r' => (1, Self::READ.bits()),
                    b'w' => (2, Self::WRITE.bits()),
                    b'x' => (3, Self::EXECUTE.bits()),
                    b'a' => (4, Self::ADOPT.bits()),
                    _ => return None,
                };
                (order > previous && bits & bit == 0).then_some((bits | bit, order))
            });
        expected
            .map(|(bits, _)| Self::from_bits_truncate(bits))
            .ok_or_else(|| InvalidPermissions(value.to_owned()))
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (allowed, letter) in [
            (self.can_read(), "r"),
            (self.can_write(), "w"),
            (self.can_execute(), "x"),
            (self.contains(Self::ADOPT), "a"),
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
