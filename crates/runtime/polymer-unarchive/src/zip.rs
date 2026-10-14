use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use crate::{Entry, Error, Format, Sink, Unarchiver};

pub(crate) struct ZipUnarchiver(zip::ZipArchive<BufReader<File>>);

impl ZipUnarchiver {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        zip::ZipArchive::new(BufReader::new(File::open(path)?))
            .map(Self)
            .map_err(|error| Error::invalid(Format::Zip, error))
    }
}

impl Unarchiver for ZipUnarchiver {
    fn entries(&mut self) -> Result<Vec<Entry>, Error> {
        let mut entries = Vec::new();
        for index in 0..self.0.len() {
            let file = self
                .0
                .by_index_raw(index)
                .map_err(|error| Error::invalid(Format::Zip, error))?;
            if !file.is_dir() {
                entries.push(Entry {
                    path: file
                        .name()
                        .map_err(|error| Error::invalid(Format::Zip, error))?
                        .into_owned(),
                    size: file.size(),
                });
            }
        }
        Ok(entries)
    }

    fn extract(&mut self, sink: &mut Sink<'_>) -> Result<(), Error> {
        for index in 0..self.0.len() {
            let mut file = self
                .0
                .by_index(index)
                .map_err(|error| Error::invalid(Format::Zip, error))?;
            if file.is_dir() {
                continue;
            }
            let name = file
                .name()
                .map_err(|error| Error::invalid(Format::Zip, error))?
                .into_owned();
            if file.is_symlink() {
                let mut target = String::new();
                file.read_to_string(&mut target)?;
                sink.symlink(&name, &target)?;
                continue;
            }
            let mode = file.unix_mode();
            sink.file(&name, mode, |output| Ok(std::io::copy(&mut file, output)?))?;
        }
        Ok(())
    }
}
