mod install;
mod metadata;
pub(in crate::impls::ctr) mod ncch;

use crate::impls::ctr::{CtrNand, layout::Layout};
use crate::{
    Error,
    installable::{Depot, DepotType, IconHint, InstallEvents, InstallPreview},
    nand::VirtualNand,
};
use csis::{CiaArchive, CiaSection};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// CIA title contents.
#[derive(Clone)]
pub struct CiaDepot {
    path: PathBuf,
    title_id: u64,
    root: String,
    title_root: String,
    content_root: String,
    kind: DepotType,
}

impl CiaDepot {
    pub fn title_id(&self) -> u64 {
        self.title_id
    }
    pub fn install_path(&self) -> &str {
        &self.root
    }

    fn title_kind(id: u64) -> DepotType {
        let category = (id >> 32) as u16;
        let nand = category & (0x10 | 0x01 | 0x8000) != 0 || id & 0x02 != 0;
        match category {
            0x000e => DepotType::AppUpdate,
            0x008c => DepotType::AppContent,
            _ if nand && category & 0x8000 == 0 => DepotType::System,
            _ => DepotType::App,
        }
    }

    fn content_path(&self, content_id: u32) -> String {
        format!("{}/{content_id:08x}.app", self.content_root)
    }

    fn verify(&self, events: &InstallEvents) -> Vec<String> {
        let mut archive = match CiaArchive::open(&self.path) {
            Ok(archive) => archive,
            Err(_) => return vec![self.path.display().to_string()],
        };
        events.phase(
            crate::installable::InstallPhase::Verifying,
            archive.items().iter().map(|item| item.size).sum(),
        );
        let mut failures = Vec::new();
        for index in 0..archive.items().len() {
            let verified = archive.stream_item_with_progress(index, &mut io::sink(), |bytes| {
                events.advance(bytes);
                Ok(())
            });
            if verified.is_err() {
                failures.push(format!("{:08x}.app", archive.items()[index].content_id));
            }
        }
        failures
    }

    fn jobs(&self, archive: &CiaArchive<File>) -> Vec<install::Entry> {
        let mut jobs: Vec<_> = archive
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| install::Entry {
                path: self.content_path(item.content_id),
                content: install::Content::Item(index),
            })
            .collect();
        // Publish the TMD only after every present content has passed its hash check.
        jobs.push(install::Entry {
            path: format!("{}/00000000.tmd", self.root),
            content: install::Content::Section(CiaSection::Tmd),
        });
        jobs
    }
}

impl Depot for CiaDepot {
    fn embedded_keys(&self) -> Result<Vec<crate::keys::KeyRecord>, Error> {
        Ok(vec![super::super::ticket::CtrTicket::from_cia(&self.path)?])
    }

    fn open(path: &Path) -> Result<Self, Error> {
        let archive = CiaArchive::open(path).map_err(|e| Error::malformed("CIA", e.to_string()))?;
        let id = archive.title_id();
        if id >> 48 != 4 || archive.items().is_empty() {
            return Err(Error::Unsupported(
                "CIA requires a 3DS title with present content".into(),
            ));
        }
        let category = (id >> 32) as u16;
        let nand = category & (0x10 | 0x01 | 0x8000) != 0 || id & 0x02 != 0;
        let base = if nand {
            Layout::NAND_TITLE.to_owned()
        } else {
            format!("{}/title", Layout::SD)
        };
        let root = format!("{base}/{:08x}/{:08x}/content", id >> 32, id as u32);
        let content_root = if archive
            .tmd()
            .contents()
            .get(1)
            .is_some_and(|r| r.content_type.is_optional())
        {
            format!("{root}/00000000")
        } else {
            root.clone()
        };
        let kind = Self::title_kind(id);
        Ok(Self {
            path: path.to_owned(),
            title_id: id,
            title_root: root.clone(),
            root,
            content_root,
            kind,
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if !(CtrNand::supports(nand.platform()) && nand.root().read_dir("nand/dbs", false).is_ok())
        {
            return Err(Error::Unsupported(
                "Depot requires a compatible NAND with its storage layout initialized".into(),
            ));
        }
        Ok(())
    }
    fn preview(&self) -> Option<InstallPreview> {
        let smdh = crate::impls::ctr::smdh::ExtdataIcon::from_game(&self.path);
        let id = format!("{:016X}", self.title_id);
        let label = smdh
            .as_ref()
            .map(|smdh| smdh.description().0)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let icon = smdh
            .and_then(|smdh| IconHint::image(smdh.rgba()).ok())
            .and_then(|icon| match icon {
                IconHint::Embedded(icon) => Some(icon.data_url()),
                _ => None,
            });
        Some(InstallPreview {
            id,
            label,
            depot_type: self.kind,
            icon,
        })
    }

    fn validate_contents(&self) -> Vec<String> {
        self.verify(&InstallEvents::default())
    }
    fn depot_type(&self) -> DepotType {
        self.kind
    }
    fn install<'a, N: VirtualNand + ?Sized>(
        self,
        nand: &'a N,
        events: InstallEvents,
    ) -> crate::installable::Installation<'a>
    where
        Self: 'a,
    {
        let source = self;
        crate::installable::Installation::prepare(async move {
            source.validate_target(nand)?;
            let verifying = source.clone();
            let verified = events.clone();
            let missing = tokio::task::spawn_blocking(move || verifying.verify(&verified))
                .await
                .map_err(|e| Error::Unsupported(format!("CIA worker failed: {e}")))?;
            if !missing.is_empty() {
                return Err(Error::InvalidDepot {
                    root: source.path.display().to_string(),
                    missing,
                });
            }
            install::CiaInstall::new(&source, nand.root(), events).await
        })
    }
}
