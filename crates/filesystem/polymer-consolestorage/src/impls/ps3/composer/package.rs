use std::collections::BTreeMap;

use crate::installable::DepotType;
use crate::nand::composition::ComponentRevision;
use crate::{ArtifactComponent, Error};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(in crate::impls::ps3) struct PkgContribution {
    pub content_id: String,
    pub title_id: String,
    pub digest: [u8; 20],
    pub app_version: Option<String>,
    pub target_app_version: Option<String>,
    pub files: BTreeMap<String, bool>,
}

impl PkgContribution {
    pub fn read(revision: &ComponentRevision) -> Result<Self, Error> {
        serde_json::from_value(revision.metadata.clone())
            .map_err(|error| Error::malformed("PS3 component", error.to_string()))
    }

    pub fn component(&self, root: &str, kind: DepotType) -> ArtifactComponent {
        let id = if kind == DepotType::AppContent {
            format!("ps3:dlc:{root}:{}", self.content_id)
        } else {
            format!("ps3:main:{root}")
        };
        ArtifactComponent {
            id,
            title: root.to_owned(),
            root: root.to_owned(),
        }
    }

    pub fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        let kind = revision.artifact.depot_type();
        let role = match kind {
            DepotType::App => "app",
            DepotType::AppUpdate => "update",
            DepotType::AppContent => "content",
            _ => return Err(Error::Unsupported("PS3 package component kind".into())),
        };
        let directory = revision
            .component
            .root
            .strip_prefix("dev_hdd0/game/")
            .filter(|directory| !directory.is_empty() && !directory.contains('/'));
        if directory.is_none()
            || super::super::TitleId::parse(&self.title_id).is_none()
            || self.content_id.get(7..16) != Some(self.title_id.as_str())
            || revision.component != self.component(&revision.component.root, kind)
            || revision.artifact.id() != format!("{}:{role}", self.content_id)
        {
            return Err(Error::HistoryConflict(
                "invalid PS3 component identity".into(),
            ));
        }
        let paths = revision
            .artifact
            .manifest()
            .iter()
            .map(|file| file.path().to_string_lossy().replace('\\', "/"))
            .collect::<std::collections::BTreeSet<_>>();
        if paths != self.files.keys().cloned().collect() {
            return Err(Error::HistoryConflict(
                "PS3 package file policies do not match its manifest".into(),
            ));
        }
        for value in self.app_version.iter().chain(&self.target_app_version) {
            Self::version(value)?;
        }
        Ok(())
    }

    pub fn accepts_version(&self, kind: DepotType, current: Option<&str>) -> Result<bool, Error> {
        let current = current.map(Self::version).transpose()?;
        if let Some(target) = &self.target_app_version {
            return Ok(current == Some(Self::version(target)?));
        }
        if kind == DepotType::AppUpdate
            && let (Some(current), Some(incoming)) = (current, self.app_version.as_deref())
        {
            return Ok(Self::version(incoming)? >= current);
        }
        Ok(true)
    }

    fn version(value: &str) -> Result<(u32, u32), Error> {
        let invalid = || Error::malformed("PS3 application version", value);
        let (major, minor) = value.split_once('.').ok_or_else(invalid)?;
        if major.is_empty()
            || minor.is_empty()
            || !major
                .bytes()
                .chain(minor.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid());
        }
        Ok((
            major.parse().map_err(|_| invalid())?,
            minor.parse().map_err(|_| invalid())?,
        ))
    }
}
