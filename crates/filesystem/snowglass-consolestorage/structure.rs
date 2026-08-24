use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::storage::{EntryKind, Storage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepotEntry {
    path: &'static str,
    kind: EntryKind,
}

impl DepotEntry {
    /// Parse a layout literal such as `"code/app.xml"` or `"meta/"`.
    pub const fn new(spec: &'static str) -> DepotEntry {
        const fn is_dots(bytes: &[u8], start: usize, end: usize) -> bool {
            match end - start {
                1 => bytes[start] == b'.',
                2 => bytes[start] == b'.' && bytes[start + 1] == b'.',
                _ => false,
            }
        }

        let bytes = spec.as_bytes();

        assert!(!bytes.is_empty(), "structure entry may not be empty");
        assert!(
            bytes[0] != b'/',
            "structure entry must be relative to the depot root"
        );

        let kind = if bytes[bytes.len() - 1] == b'/' {
            EntryKind::Directory
        } else {
            EntryKind::File
        };

        // Strip the trailing marker so the entry can be joined as-is.
        let (path, len) = match kind {
            EntryKind::Directory => (
                // SAFETY: `/` is ASCII, so the truncation lands on a char boundary.
                unsafe { str::from_utf8_unchecked(bytes.split_at(bytes.len() - 1).0) },
                bytes.len() - 1,
            ),
            EntryKind::File => (spec, bytes.len()),
        };

        assert!(len > 0, "structure entry may not be empty");

        // Reject anything that would make the join escape the root or depend on
        // platform path parsing.
        let mut i = 0;
        let mut component_start = 0;
        while i < len {
            let b = bytes[i];
            assert!(b != b'\\', "structure entries must use `/` as a separator");
            if b == b'/' {
                assert!(
                    i > component_start,
                    "structure entry has an empty component"
                );
                assert!(
                    !is_dots(bytes, component_start, i),
                    "structure entries may not contain `.` or `..`"
                );
                component_start = i + 1;
            }
            i += 1;
        }
        assert!(
            len > component_start,
            "structure entry has an empty component"
        );
        assert!(
            !is_dots(bytes, component_start, len),
            "structure entries may not contain `.` or `..`"
        );

        DepotEntry { path, kind }
    }

    /// The normalized, `/`-separated path relative to the depot root.
    pub const fn path(&self) -> &'static str {
        self.path
    }

    /// Whether this entry must be a file or a directory.
    pub const fn kind(&self) -> EntryKind {
        self.kind
    }

    /// The entry as originally declared, i.e. directories keep their trailing `/`.
    fn declared(&self) -> String {
        match self.kind {
            EntryKind::Directory => format!("{}/", self.path),
            EntryKind::File => self.path.to_string(),
        }
    }
}

/// An entry to create, optionally seeded with contents.
#[derive(Debug, Clone, Copy)]
pub struct StructureEntry<'a> {
    entry: DepotEntry,
    contents: Option<&'a [u8]>,
}

impl<'a> StructureEntry<'a> {
    pub const fn new(entry: DepotEntry, contents: Option<&'a [u8]>) -> Self {
        Self { entry, contents }
    }
}

/// Something a layout can be asserted against or created under: a host
/// directory or a [`Storage`].
pub trait StructureRoot {
    fn label(&self) -> Cow<'_, str>;
    fn has(&self, entry: &DepotEntry) -> bool;

    /// Create `entry` if it is not already there. Existing entries are left alone.
    fn ensure(&self, entry: &DepotEntry, contents: Option<&[u8]>) -> Result<(), Error>;
}

impl StructureRoot for Path {
    fn label(&self) -> Cow<'_, str> {
        self.to_string_lossy()
    }

    fn has(&self, entry: &DepotEntry) -> bool {
        std::fs::metadata(self.join(entry.path())).is_ok_and(|meta| entry.kind().matches(&meta))
    }

    fn ensure(&self, entry: &DepotEntry, contents: Option<&[u8]>) -> Result<(), Error> {
        if self.has(entry) {
            return Ok(());
        }

        let target = self.join(entry.path());
        match entry.kind() {
            EntryKind::Directory => {
                std::fs::create_dir_all(&target).map_err(|e| Error::io(&target, e))
            }
            EntryKind::File => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                }
                std::fs::write(&target, contents.unwrap_or_default())
                    .map_err(|e| Error::io(&target, e))
            }
        }
    }
}

impl StructureRoot for PathBuf {
    fn label(&self) -> Cow<'_, str> {
        self.as_path().label()
    }

    fn has(&self, entry: &DepotEntry) -> bool {
        self.as_path().has(entry)
    }

    fn ensure(&self, entry: &DepotEntry, contents: Option<&[u8]>) -> Result<(), Error> {
        self.as_path().ensure(entry, contents)
    }
}

impl StructureRoot for dyn Storage + '_ {
    fn label(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{self:?}"))
    }

    fn has(&self, entry: &DepotEntry) -> bool {
        self.metadata(entry.path())
            .is_ok_and(|meta| meta.kind == entry.kind())
    }

    fn ensure(&self, entry: &DepotEntry, contents: Option<&[u8]>) -> Result<(), Error> {
        if self.has(entry) {
            return Ok(());
        }

        match entry.kind() {
            EntryKind::Directory => self.create_dir_all(entry.path()),
            EntryKind::File => self
                .create(entry.path())?
                .write_all_at(0, contents.unwrap_or_default()),
        }
    }
}

/// Assert that every entry exists under `root` with the declared kind.
pub fn require_structure<R: StructureRoot + ?Sized>(
    root: &R,
    entries: &[DepotEntry],
) -> Result<(), Error> {
    let missing: Vec<String> = entries
        .iter()
        .filter(|entry| !root.has(entry))
        .map(DepotEntry::declared)
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidDepot {
            root: root.label().into_owned(),
            missing,
        })
    }
}

/// Create every entry that is not already present, in order.
pub fn ensure_structure<R: StructureRoot + ?Sized>(
    root: &R,
    entries: &[StructureEntry<'_>],
) -> Result<(), Error> {
    for entry in entries {
        root.ensure(&entry.entry, entry.contents)?;
    }

    Ok(())
}

/// Assert a static directory layout under `root`, which may be a host `&Path` or
/// a `&dyn `[`Storage`](crate::storage::Storage).
///
/// Each entry is a path relative to `root`, `/`-separated, with a trailing `/`
/// marking a directory:
///
/// ```ignore
/// requires_structure!(path,
///     "meta/",
///     "meta/meta.xml",
///     "code/",
///     "code/app.xml",
///     "code/cos.xml",
///     "content/")?;
/// ```
///
/// The entry list is parsed and validated at compile time.
#[macro_export]
macro_rules! requires_structure {
    ($root:expr, $($entry:literal),+ $(,)?) => {
        $crate::structure::require_structure(
            $root,
            const { &[$($crate::structure::DepotEntry::new($entry)),+] },
        )
    };
}

/// Create a static layout under `root`, which may be a host `&Path` or a
/// `&dyn `[`Storage`](crate::storage::Storage). Entries that already exist with
/// the declared kind are left untouched.
///
/// A trailing `/` marks a directory, as in
/// [`requires_structure!`](crate::requires_structure). A file may be seeded with
/// contents by following it with `: expr`, where the expression is anything that
/// is `AsRef<[u8]>`:
///
/// ```ignore
/// ensure_structure!(storage,
///     "sys/",
///     "sys/title/0005001b/1005c000/content/country.txt": COUNTRY_FILE,
///     "usr/title/00050000/")?;
/// ```
#[macro_export]
macro_rules! ensure_structure {
    ($root:expr, $($entry:literal $(: $contents:expr)?),+ $(,)?) => {
        $crate::structure::ensure_structure(
            $root,
            &[$(
                $crate::structure::StructureEntry::new(
                    const { $crate::structure::DepotEntry::new($entry) },
                    {
                        #[allow(unused_mut, unused_assignments)]
                        let mut contents: ::core::option::Option<&[u8]> =
                            ::core::option::Option::None;
                        $(
                            contents = ::core::option::Option::Some(
                                ::core::convert::AsRef::<[u8]>::as_ref($contents),
                            );
                        )?
                        contents
                    },
                )
            ),+],
        )
    };
}
