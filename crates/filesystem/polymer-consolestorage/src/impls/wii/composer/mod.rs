mod state;

use std::collections::{BTreeMap, BTreeSet};

use crate::installable::{DepotFile, DepotType, artifacts::Artifact};
use crate::nand::composition::{ComponentRevision, Composition, FileContribution, NandComposer};
use crate::{ArtifactComponent, Error};
use state::WiiState;

pub(in crate::impls) struct WiiComposer;

#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::impls::wii) struct WiiContribution {
    pub title_id: u64,
    pub required_ios: u64,
    pub shared: BTreeMap<String, [u8; 20]>,
}

impl WiiContribution {
    fn read(revision: &ComponentRevision) -> Result<Self, Error> {
        serde_json::from_value(revision.metadata.clone())
            .map_err(|error| Error::malformed("Wii component", error.to_string()))
    }
}

impl WiiComposer {
    pub(in crate::impls::wii) fn component(title_id: u64) -> ArtifactComponent {
        let id = format!("{title_id:016x}");
        ArtifactComponent {
            id: id.clone(),
            title: id,
            root: format!("{}/content", super::title_path(title_id)),
        }
    }

    pub(in crate::impls::wii) fn title_kind(title_id: u64) -> DepotType {
        match title_id >> 32 {
            1 => DepotType::System,
            0x10005 => DepotType::AppContent,
            _ => DepotType::App,
        }
    }
}

impl NandComposer for WiiComposer {
    fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        let contribution = WiiContribution::read(revision)?;
        if revision.component != Self::component(contribution.title_id)
            || revision.artifact.id() != format!("{:016x}", contribution.title_id)
            || revision.artifact.depot_type() != Self::title_kind(contribution.title_id)
        {
            return Err(Error::HistoryConflict(
                "invalid Wii component identity".into(),
            ));
        }
        let paths = revision
            .artifact
            .manifest()
            .iter()
            .map(|file| file.path().to_string_lossy().replace('\\', "/"))
            .collect::<BTreeSet<_>>();
        if !paths.contains(&format!("{}/title.tmd", revision.component.root))
            || contribution
                .shared
                .keys()
                .any(|path| !paths.contains(path) || !path.ends_with(".app"))
        {
            return Err(Error::HistoryConflict(
                "invalid Wii content manifest".into(),
            ));
        }
        Ok(())
    }

    fn compose(
        &self,
        revisions: &[ComponentRevision],
        state: &serde_json::Value,
    ) -> Result<Composition, Error> {
        let mut state = WiiState::read(state)?;
        let mut output = Composition::disjoint(revisions)?;
        let mut shared = BTreeMap::<[u8; 20], DepotFile>::new();
        for revision in revisions {
            let contribution = WiiContribution::read(revision)?;
            state.insert_title(contribution.title_id)?;
            let mut manifest = BTreeMap::new();
            for file in revision.artifact.manifest() {
                let path = file.path().to_string_lossy().replace('\\', "/");
                let Some(hash) = contribution.shared.get(&path) else {
                    manifest.insert(path, file.clone());
                    continue;
                };
                let destination = state.shared_path(hash)?;
                if shared.get(hash).is_some_and(|old| {
                    old.blake3() != file.blake3() || old.byte_length() != file.byte_length()
                }) {
                    return Err(Error::HistoryConflict(
                        "Wii shared-content hash collision".into(),
                    ));
                }
                let installed = DepotFile::new(
                    destination.clone().into(),
                    *file.blake3(),
                    file.byte_length(),
                );
                shared.insert(*hash, installed.clone());
                manifest.insert(destination.clone(), installed);
                output.files.remove(&path);
                output.files.insert(
                    destination,
                    FileContribution {
                        revision: revision.revision.clone(),
                        path,
                    },
                );
            }
            let artifact = &revision.artifact;
            let installed = Artifact::new(
                artifact.id().to_owned(),
                artifact.depot_type(),
                manifest.into_values().collect(),
                artifact.label().to_owned(),
                artifact.version().map(str::to_owned),
                artifact.icons().to_vec(),
                artifact.entrypoint().cloned(),
            )?
            .with_installed(artifact.installed());
            output.artifacts.insert(artifact.id().to_owned(), installed);
        }
        output
            .generated
            .insert("sys/uid.sys".into(), state.uid_bytes());
        output.generated.insert(
            "shared1/content.map".into(),
            state.shared_bytes(shared.keys()),
        );
        output.state = serde_json::to_value(state)
            .map_err(|error| Error::malformed("Wii composer state", error.to_string()))?;
        output.validate()?;
        Ok(output)
    }

    fn affected(&self, revisions: &[ComponentRevision], ids: &[&str]) -> Vec<String> {
        revisions
            .iter()
            .filter_map(|revision| {
                let contribution = WiiContribution::read(revision).ok()?;
                (contribution.required_ios >> 32 == 1
                    && ids.contains(&format!("{:016x}", contribution.required_ios).as_str())
                    && !ids.contains(&revision.artifact.id()))
                .then(|| revision.artifact.id().to_owned())
            })
            .collect()
    }
}
