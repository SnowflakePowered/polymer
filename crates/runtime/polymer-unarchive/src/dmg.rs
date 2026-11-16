use std::{
    fs::File,
    io::{BufWriter, Seek, SeekFrom},
    path::Path,
};

use hfsplus::{EntryKind, HfsVolume, WalkEntry};

use crate::{Entry, Error, Format, Sink, Unarchiver};

/// The HFS+ volume of a disk image, decompressed into a temporary file.
pub(crate) struct DmgUnarchiver(HfsVolume<File>);

impl DmgUnarchiver {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let invalid = |error: udif::DppError| Error::invalid(Format::Dmg, error);
        let mut image = udif::open(path).map_err(invalid)?;
        let partition = image.hfs_partition_id().map_err(invalid)?;
        let mut volume = BufWriter::new(tempfile::tempfile()?);
        image
            .decompress_partition_to(partition, &mut volume)
            .map_err(invalid)?;
        let mut volume = volume.into_inner().map_err(|error| error.into_error())?;
        volume.seek(SeekFrom::Start(0))?;
        HfsVolume::open(volume)
            .map(Self)
            .map_err(|error| Error::invalid(Format::Dmg, error))
    }

    fn extracted(entry: &WalkEntry) -> bool {
        let first = entry
            .path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default();
        match entry.entry.kind {
            EntryKind::Directory => false,
            EntryKind::Symlink if entry.path == "/Applications" => false,
            _ => {
                !first.starts_with('\0')
                    && !first.starts_with(".HFS+ Private")
                    && !first.starts_with(".journal")
            }
        }
    }

    fn walk(&mut self) -> Result<Vec<WalkEntry>, Error> {
        let mut entries = self
            .0
            .walk()
            .map_err(|error| Error::invalid(Format::Dmg, error))?;
        entries.retain(Self::extracted);
        Ok(entries)
    }
}

impl Unarchiver for DmgUnarchiver {
    fn entries(&mut self) -> Result<Vec<Entry>, Error> {
        Ok(self
            .walk()?
            .into_iter()
            .map(|entry| Entry {
                path: entry.path,
                size: entry.entry.size,
            })
            .collect())
    }

    fn extract(&mut self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let invalid = |error| Error::invalid(Format::Dmg, error);
        for entry in self.walk()? {
            if entry.entry.kind == EntryKind::Symlink {
                let target = self.0.read_file(&entry.path).map_err(invalid)?;
                sink.symlink(&entry.path, &String::from_utf8_lossy(&target))?;
                continue;
            }
            let mode = self.0.stat(&entry.path).map_err(invalid)?.permissions.mode;
            let volume = &mut self.0;
            sink.file(&entry.path, Some(u32::from(mode)), |output| {
                volume.read_file_to(&entry.path, output).map_err(invalid)
            })?;
        }
        Ok(())
    }
}
