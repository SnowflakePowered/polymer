use super::*;

#[derive(Clone)]
pub(super) struct GitArtifactRegistry(pub(super) Arc<GitVolumeInner>);

impl GitArtifactRegistry {
    pub(super) fn storage(&self) -> GitVolumeStorage {
        GitVolumeStorage {
            inner: self.0.clone(),
            artifacts: self.clone(),
        }
    }
}

impl ArtifactRegistry for GitArtifactRegistry {
    fn revision(&self) -> Result<String, Error> {
        let repo = self.0.platform.repository().to_thread_local();
        let reference = self.storage().ref_name();
        Ok(ArtifactRepository::new(&repo, &reference)
            .tip()?
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
            let reference = self.storage().ref_name();
            let artifacts = ArtifactRepository::new(&repo, &reference);
            let Some(tip) = artifacts.tip()? else {
                return Ok(Vec::new());
            };
            let history = artifacts.linear_history(tip)?;
            let records = artifacts.records()?;
            let installed = records
                .iter()
                .map(|record| record.id.clone())
                .collect::<BTreeSet<_>>();
            let removed = ids
                .iter()
                .filter(|id| installed.contains(**id))
                .map(|id| (*id).to_owned())
                .collect::<BTreeSet<_>>();
            Ok(artifacts
                .affected_depots(&history, &records, &removed)?
                .into_iter()
                .collect())
        })
    }

    fn record<'a>(
        &'a self,
        artifact: &'a Artifact,
        context: &'a crate::installable::InstallContext,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.0.working.write().metadata.artifacts.insert(
                artifact.id().to_owned(),
                Some(ArtifactRecord::from(artifact)),
            );
            let storage = self.storage();
            let message = format!("Polymer install {}", artifact.id());
            context
                .spawn_blocking(move |context| {
                    storage.commit_pending_reporting(&message, None, context)
                })
                .await
                .map_err(|error| Error::Unsupported(format!("commit worker failed: {error}")))??;
            Ok(())
        })
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(async move {
            let repo = self.0.platform.repository().to_thread_local();
            let reference = self.storage().ref_name();
            let mut summaries = ArtifactRepository::new(&repo, &reference).summaries()?;
            summaries.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(summaries)
        })
    }

    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>> {
        Box::pin(async move {
            let repo = self.0.platform.repository().to_thread_local();
            let reference = self.storage().ref_name();
            ArtifactRepository::new(&repo, &reference)
                .get(id)?
                .map(|tree| tree.record()?.artifact())
                .transpose()
        })
    }

    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>> {
        Box::pin(async move {
            let repo = self.0.platform.repository().to_thread_local();
            let reference = self.storage().ref_name();
            Ok(ArtifactRepository::new(&repo, &reference)
                .get(id)?
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
            let reference = storage.ref_name();
            let artifacts = ArtifactRepository::new(&repo, &reference);
            let Some(tip) = artifacts.tip()? else {
                return Ok(());
            };
            let records = artifacts.records()?;
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
            let history = artifacts.linear_history(tip)?;
            let affected = artifacts.affected_depots(&history, &records, &removed)?;
            if !affected.is_empty() {
                return Err(Error::AffectedDepots {
                    affected: affected.into_iter().collect(),
                });
            }
            let retained = ArtifactRepository::retained_files(&records, &removed);
            let new_tip = artifacts.rewrite_without(&history, &removed, &retained, tip)?;
            storage.refresh_checkout_locked(&repo, Some(new_tip))
        })
    }
}
