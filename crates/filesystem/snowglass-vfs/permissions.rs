use crate::{VfsError, VfsResult};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Permissions(u8);

impl Permissions {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);
    pub const ALL: Self = Self(7);

    pub const fn contains(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }

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
    type Err = VfsError;

    fn from_str(value: &str) -> VfsResult<Self> {
        let expected = value
            .bytes()
            .try_fold((0_u8, 0_u8), |(bits, previous), byte| {
                let (order, bit) = match byte {
                    b'r' => (1, Self::READ.0),
                    b'w' => (2, Self::WRITE.0),
                    b'x' => (3, Self::EXECUTE.0),
                    _ => return None,
                };
                (order > previous && bits & bit == 0).then_some((bits | bit, order))
            });
        expected
            .map(|(bits, _)| Self(bits))
            .ok_or_else(|| VfsError::InvalidPermissions(value.to_owned()))
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.can_read() {
            formatter.write_str("r")?;
        }
        if self.can_write() {
            formatter.write_str("w")?;
        }
        if self.can_execute() {
            formatter.write_str("x")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_canonical_subsets() {
        for mode in ["", "r", "w", "x", "rw", "rx", "wx", "rwx"] {
            assert_eq!(mode.parse::<Permissions>().unwrap().to_string(), mode);
        }
        for mode in ["wr", "rr", "rxx", "a", "rwxr"] {
            assert!(mode.parse::<Permissions>().is_err(), "{mode}");
        }
    }
}
