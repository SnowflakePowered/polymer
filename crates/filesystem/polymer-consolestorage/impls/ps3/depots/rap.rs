use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use super::super::{GUEST_USER_ROOT, Ps3Nand, TitleId};
use crate::Error;
use crate::installable::artifacts::Artifact;
use crate::installable::{Depot, DepotFile, DepotType, IconHint, Installable};
use crate::nand::VirtualNand;

/// A standalone 16-byte RAP license named `<CONTENT_ID>.rap`.
pub struct Ps3RapDepot {
    content_id: String,
    bytes: [u8; 16],
}

impl Ps3RapDepot {
    pub fn content_id(&self) -> &str {
        &self.content_id
    }

    pub fn install_path(&self) -> String {
        format!("{GUEST_USER_ROOT}/exdata/{}.rap", self.content_id)
    }
}

impl Depot for Ps3RapDepot {
    fn open(path: &Path) -> Result<Self, Error> {
        let content_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
        let id = content_id.as_bytes();
        if id.len() != 36
            || !id
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || id[6] != b'-'
            || id[16] != b'_'
            || !id[17..19].iter().all(u8::is_ascii_digit)
            || id[19] != b'-'
            || TitleId::parse(&content_id[7..16]).is_none()
        {
            return Err(Error::malformed(
                path.display().to_string(),
                "RAP filename must contain a PS3 content ID",
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)
            .and_then(|file| file.take(17).read_to_end(&mut bytes))
            .map_err(|error| Error::io(path, error))?;
        let bytes = bytes.try_into().map_err(|_| {
            Error::malformed(path.display().to_string(), "a RAP must be exactly 16 bytes")
        })?;
        Ok(Self {
            content_id: content_id.to_owned(),
            bytes,
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if nand.is_dirty()? {
            return Err(Error::HistoryConflict(
                "This NAND has uncommitted changes. Recover it in Console storage before installing.".into(),
            ));
        }
        if !(nand.platform() == Ps3Nand::PLATFORM && Ps3Nand::has_structure(&*nand.root())) {
            return Err(Error::Unsupported(
                "Depot requires a compatible NAND with its storage layout initialized".into(),
            ));
        }
        if !nand.is_complete(crate::nand::NandCompletion::Minimal) {
            return Err(Error::Unsupported(
                "Install PlayStation 3 system software before using this NAND".into(),
            ));
        }
        Ok(())
    }
    fn validate_contents(&self) -> Vec<String> {
        // Length is checked on open. A RAP has no embedded integrity digest.
        Vec::new()
    }

    fn depot_type(&self) -> DepotType {
        DepotType::AppTicket
    }

    fn install<'a, N: VirtualNand + ?Sized>(
        self,
        nand: &'a N,
    ) -> crate::installable::Installation<'a>
    where
        Self: 'a,
    {
        let source = self;

        crate::installable::Installation::prepare(async move {
            source.satisfies(nand)?;
            Ok(RapInstall {
                source,
                storage: nand.root(),
                written: false,
            })
        })
    }
}

struct RapInstall {
    source: Ps3RapDepot,
    storage: std::sync::Arc<dyn crate::VolumeStorage>,
    written: bool,
}

impl Installable for RapInstall {
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
        if self.written {
            return Ok(None);
        }
        let path = self.source.install_path();
        let copied = self
            .storage
            .install_file(Box::new(Cursor::new(self.source.bytes)), &path)
            .await?;
        self.written = true;
        Ok(Some(vec![DepotFile::new(
            PathBuf::from(path),
            copied.blake3,
            copied.byte_length,
        )]))
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let artifact = Artifact::new(
            format!("{}:rap", self.source.content_id),
            DepotType::AppTicket,
            manifest,
            format!("{} license", self.source.content_id),
            None,
            IconHint::Builtin(DepotType::AppTicket),
        );
        self.storage.artifacts().record(&artifact).await?;
        Ok(artifact)
    }
}
