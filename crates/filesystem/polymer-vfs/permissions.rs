use crate::{VfsError, VfsResult};
use std::{fmt, str::FromStr};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    pub struct Permissions: u8 {
        const READ = 1;
        const WRITE = 2;
        const EXECUTE = 4;
        /// The backend can adopt a source node as the file at this path.
        const ADOPT = 8;
    }
}

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
    type Err = VfsError;

    fn from_str(value: &str) -> VfsResult<Self> {
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
        if self.contains(Self::ADOPT) {
            formatter.write_str("a")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_canonical_subsets() {
        for mode in [
            "", "r", "w", "x", "a", "rw", "rx", "ra", "wx", "wa", "xa", "rwx", "rwa", "rxa", "wxa",
            "rwxa",
        ] {
            assert_eq!(mode.parse::<Permissions>().unwrap().to_string(), mode);
        }
        for mode in ["wr", "rr", "rxx", "aa", "p", "rwxr", "rwxar"] {
            assert!(mode.parse::<Permissions>().is_err(), "{mode}");
        }
    }
}
