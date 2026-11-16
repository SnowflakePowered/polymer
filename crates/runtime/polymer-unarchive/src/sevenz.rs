use std::{fs::File, path::Path};

use sevenz_rust2::{ArchiveReader, Password};

use crate::{Entry, Error, Format, Sink, Unarchiver};

pub(crate) struct SevenZipUnarchiver(ArchiveReader<File>);

impl SevenZipUnarchiver {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        ArchiveReader::open(path, Password::empty())
            .map(Self)
            .map_err(|error| Error::invalid(Format::SevenZip, error))
    }
}

impl Unarchiver for SevenZipUnarchiver {
    fn entries(&mut self) -> Result<Vec<Entry>, Error> {
        Ok(self
            .0
            .archive()
            .files
            .iter()
            .filter(|entry| !entry.is_directory() && !entry.is_anti_item())
            .map(|entry| Entry {
                path: entry.name().to_owned(),
                size: entry.size(),
            })
            .collect())
    }

    fn extract(&mut self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut failure = None;
        let result = self.0.for_each_entries(|entry, reader| {
            if entry.is_directory() || entry.is_anti_item() {
                return Ok(true);
            }
            match sink.file(entry.name(), None, |output| {
                Ok(std::io::copy(reader, output)?)
            }) {
                Ok(()) => Ok(true),
                Err(error) => {
                    failure = Some(error);
                    Ok(false)
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        result.map_err(|error| Error::invalid(Format::SevenZip, error))
    }
}
