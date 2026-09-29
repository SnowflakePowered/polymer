use super::*;

#[derive(Clone)]
pub(super) struct GitArtifactRegistry(pub(super) Arc<GitVolumeInner>);

impl ArtifactRegistry for GitArtifactRegistry {
    fn revision(&self) -> Result<String, Error> {
        let repo = self.0.platform.repository().to_thread_local();
        Ok(GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())?
            .map(|id| id.to_string())
            .unwrap_or_default())
    }
    fn affected<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<String>, Error>> {
        Box::pin(async move {
            let _guard = self.0.platform.0.operation.lock();
            if !self.0.working.read().is_empty() {
                return Err(Error::HistoryConflict(
                    "NAND has uncommitted changes".into(),
                ));
            }
            let repo = self.0.platform.repository().to_thread_local();
            let Some(tip) = GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())? else {
                return Ok(Vec::new());
            };
            let history = Self::linear_history(&repo, tip)?;
            let records = self.records()?;
            let installed = records
                .iter()
                .map(|record| record.id.clone())
                .collect::<BTreeSet<_>>();
            let removed = ids
                .iter()
                .filter(|id| installed.contains(**id))
                .map(|id| (*id).to_owned())
                .collect::<BTreeSet<_>>();
            Ok(Self::affected_depots(&repo, &history, &records, &removed)?
                .into_iter()
                .collect())
        })
    }

    fn record<'a>(
        &'a self,
        artifact: &'a Artifact,
        events: InstallEvents,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.0.working.write().metadata.artifacts.insert(
                artifact.id().to_owned(),
                Some(ArtifactRecord::from(artifact)),
            );
            let storage = self.storage();
            let message = format!("Polymer install {}", artifact.id());
            tokio::task::spawn_blocking(move || {
                storage.commit_pending_reporting(&message, None, &events)
            })
            .await
            .map_err(|error| Error::Unsupported(format!("commit worker failed: {error}")))??;
            Ok(())
        })
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(async move {
            let mut summaries = self.read_artifacts(read_artifact_summary)?;
            summaries.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(summaries)
        })
    }

    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>> {
        Box::pin(async move {
            let repo = self.0.platform.repository().to_thread_local();
            let tip = GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())?;
            let root = GitVolumeStorage::root_tree(&repo, tip)?;
            let Some(entry) = root
                .lookup_entry_by_path(format!("meta/artifacts/{}", artifact_key(id)))
                .map_err(|error| Error::git("lookup artifact", error))?
            else {
                return Ok(None);
            };
            let tree = repo
                .find_tree(entry.oid())
                .map_err(|error| Error::git("read artifact tree", error))?;
            read_artifact(&repo, &tree)?.artifact().map(Some)
        })
    }

    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>> {
        Box::pin(async move {
            let repo = self.0.platform.repository().to_thread_local();
            let tip = GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())?;
            Ok(GitVolumeStorage::root_tree(&repo, tip)?
                .lookup_entry_by_path(format!("meta/artifacts/{}", artifact_key(id)))
                .map_err(|error| Error::git("lookup artifact", error))?
                .is_some())
        })
    }

    fn uninstall<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let requested = ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect::<BTreeSet<_>>();
            if requested.is_empty() {
                return Ok(());
            }
            let _guard = self.0.platform.0.operation.lock();
            if !self.0.working.read().is_empty() {
                return Err(Error::HistoryConflict(
                    "cannot uninstall depots while the NAND has uncommitted changes".to_owned(),
                ));
            }
            let repo = self.0.platform.repository().to_thread_local();
            let storage = self.storage();
            let Some(tip) = GitVolumeStorage::tip_in(&repo, &storage.ref_name())? else {
                return Ok(());
            };
            let records = self.records()?;
            let installed = records
                .iter()
                .map(|record| record.id.clone())
                .collect::<BTreeSet<_>>();
            let removed = requested
                .intersection(&installed)
                .cloned()
                .collect::<BTreeSet<_>>();
            if removed.is_empty() {
                return Ok(());
            }
            let history = Self::linear_history(&repo, tip)?;
            let affected = Self::affected_depots(&repo, &history, &records, &removed)?;
            if !affected.is_empty() {
                return Err(Error::AffectedDepots {
                    affected: affected.into_iter().collect(),
                });
            }
            let retained = Self::retained_files(&records, &removed);
            self.rewrite_without(&repo, &history, &removed, &retained, tip)
        })
    }
}
