mod install;

use crate::impls::ctr::{CtrNand, layout::Layout};
use crate::{
    Error,
    installable::{
        Depot, DepotType, IconHint,
        artifacts::{Artifact, DescribeArtifact},
    },
    nand::VirtualNand,
};
use csis::{CiaArchive, CiaSection};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// CIA title contents or an independently installable ticket from the same CIA.
/// `open()` selects contents; `ticket_depot()` selects only the ticket.
#[derive(Clone)]
pub struct CiaDepot {
    path: PathBuf,
    title_id: u64,
    ticket_id: u64,
    version: u16,
    root: String,
    content_root: String,
    kind: DepotType,
}

impl CiaDepot {
    pub fn open(path: &Path) -> Result<Self, Error> {
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
        let kind = match category {
            0x000e => DepotType::AppUpdate,
            0x008c => DepotType::AppContent,
            _ if nand && category & 0x8000 == 0 => DepotType::System,
            _ => DepotType::App,
        };
        Ok(Self {
            path: path.to_owned(),
            title_id: id,
            ticket_id: archive.ticket().ticket_id(),
            version: archive.tmd().title_version(),
            root,
            content_root,
            kind,
        })
    }

    /// Select the ticket as a separate artifact. Installing contents never installs it.
    pub fn ticket_depot(&self) -> Self {
        let mut depot = self.clone();
        depot.kind = DepotType::AppTicket;
        depot.root = format!(
            "nand/dbs/ticket.db/{:016X}.{:016X}.tik",
            self.title_id, self.ticket_id
        );
        depot
    }

    pub fn title_id(&self) -> u64 {
        self.title_id
    }
    pub fn install_path(&self) -> &str {
        &self.root
    }

    fn content_path(&self, content_id: u32) -> String {
        format!("{}/{content_id:08x}.app", self.content_root)
    }

    fn jobs(&self, archive: &CiaArchive<File>) -> Vec<install::Entry> {
        if self.kind == DepotType::AppTicket {
            return vec![install::Entry {
                path: self.root.clone(),
                content: install::Content::Section(CiaSection::Ticket),
            }];
        }
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

impl DescribeArtifact for CiaDepot {
    fn describe(&self) -> Artifact {
        let id = if self.kind == DepotType::AppTicket {
            format!("{:016X}:{:016X}:ticket", self.title_id, self.ticket_id)
        } else {
            format!("{:016X}:contents", self.title_id)
        };
        Artifact::new(
            id,
            self.kind,
            Vec::new(),
            format!("{:016X}", self.title_id),
            Some(self.version.to_string()),
            IconHint::Builtin(self.kind),
        )
    }
}

impl Depot for CiaDepot {
    fn satisfies<N: VirtualNand>(&self, nand: &N) -> bool {
        CtrNand::supports(nand.platform()) && nand.root().read_dir("nand/dbs", false).is_ok()
    }
    fn validate_contents(&self) -> Vec<String> {
        let mut archive = match CiaArchive::open(&self.path) {
            Ok(archive) => archive,
            Err(_) => return vec![self.path.display().to_string()],
        };
        if self.kind == DepotType::AppTicket {
            return Vec::new();
        }
        let mut failures = Vec::new();
        for index in 0..archive.items().len() {
            if archive.stream_item(index, &mut io::sink()).is_err() {
                failures.push(format!("{:08x}.app", archive.items()[index].content_id));
            }
        }
        failures
    }
    fn depot_type(&self) -> DepotType {
        self.kind
    }
    fn install<'a, N: VirtualNand>(&'a self, nand: &'a N) -> crate::installable::Installation<'a> {
        crate::installable::Installation::prepare(async move {
            if self.satisfies(nand) {
                install::CiaInstall::new(self, nand.root()).await
            } else {
                Err(Error::Unsupported(format!(
                    "CIA cannot be installed to {}",
                    nand.platform()
                )))
            }
        })
    }
}
