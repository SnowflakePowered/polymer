use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use futures::channel::oneshot;
use futures::stream;

use super::super::{GUEST_USER_ROOT, Ps3Nand, TitleId};
use crate::Error;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::installable::{Depot, DepotFile, DepotType, IconHint};
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

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> bool {
        nand.platform() == Ps3Nand::PLATFORM && Ps3Nand::has_structure(&*nand.root())
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

        let (sender, receiver) = oneshot::channel();
        let operation = stream::once(async move {
            let result: Result<_, Error> = async {
                let path = source.install_path();
                let copied = nand
                    .root()
                    .install_file(Box::new(Cursor::new(source.bytes)), &path)
                    .await?;
                let artifact = source.describe().with_manifest(vec![DepotFile::new(
                    PathBuf::from(&path),
                    copied.blake3,
                    copied.byte_length,
                )]);
                nand.record(&artifact).await?;
                Ok((artifact, path))
            }
            .await;
            match result {
                Ok((artifact, path)) => {
                    let _ = sender.send(Ok(artifact));
                    Ok(path)
                }
                Err(error) => {
                    let _ = sender.send(Err(error.clone()));
                    Err(error)
                }
            }
        });
        crate::installable::Installation::from_progress(operation, async move {
            receiver.await.unwrap_or(Err(Error::InstallIncomplete))
        })
    }
}

impl DescribeArtifact for Ps3RapDepot {
    fn describe(&self) -> Artifact {
        Artifact::new(
            format!("{}:rap", self.content_id),
            DepotType::AppTicket,
            Vec::new(),
            format!("{} license", self.content_id),
            None,
            IconHint::Builtin(DepotType::AppTicket),
        )
    }
}
