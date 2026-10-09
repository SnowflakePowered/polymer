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
    fn install_component<'a>(
        &'a self,
        component: &'a crate::ArtifactComponent,
        metadata: &'a serde_json::Value,
        artifact: &'a Artifact,
        source: Arc<dyn Storage>,
        context: &'a InstallContext,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let storage = self.storage();
            let component = component.clone();
            let metadata = metadata.clone();
            let artifact = artifact.clone();
            context
                .spawn_blocking(move |context| {
                    let _guard = storage.inner.platform.0.operation.lock();
                    let repo = storage.inner.platform.repository().to_thread_local();
                    component::ComponentHistory::new(&storage, &repo)
                        .install(&component, &metadata, &artifact, &*source, context)
                })
                .await
                .map_err(|error| Error::Unsupported(format!("component worker failed: {error}")))?
        })
    }

    fn history<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::ArtifactRevision>, Error>> {
        Box::pin(async move {
            let storage = self.storage();
            if !storage.has_component_histories() {
                return Err(Error::Unsupported("component history".into()));
            }
            let _guard = self.0.platform.0.operation.lock();
            let repo = self.0.platform.repository().to_thread_local();
            component::ComponentHistory::new(&storage, &repo).history(id)
        })
    }

    fn restore<'a>(&'a self, id: &'a str, revision: &'a str) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let storage = self.storage();
            if !storage.has_component_histories() {
                return Err(Error::Unsupported("component restore".into()));
            }
            let _guard = self.0.platform.0.operation.lock();
            let repo = self.0.platform.repository().to_thread_local();
            component::ComponentHistory::new(&storage, &repo).restore(id, revision)
        })
    }

    fn revision(&self) -> Result<String, Error> {
        self.storage().recover_components()?;
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
            let storage = self.storage();
            if storage.has_component_histories() {
                return component::ComponentHistory::new(&storage, &repo).affected(ids);
            }
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
            if self.storage().has_component_histories() {
                return Err(Error::Unsupported(
                    "artifacts for this NAND must be installed as staged components".into(),
                ));
            }
            self.0.working.write().metadata.artifacts.insert(
                artifact.id().to_owned(),
                Some(ArtifactRecord::from(artifact)),
            );
            let storage = self.storage();
            let message = format!("Polymer install {}", artifact.id());
            context
                .spawn_blocking(move |context| {
                    storage.commit_scoped(&message, context, PendingScope::All)
                })
                .await
                .map_err(|error| Error::Unsupported(format!("commit worker failed: {error}")))??;
            Ok(())
        })
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(async move {
            self.storage().recover_components()?;
            let repo = self.0.platform.repository().to_thread_local();
            let reference = self.storage().ref_name();
            let mut summaries = ArtifactRepository::new(&repo, &reference).summaries()?;
            summaries.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(summaries)
        })
    }

    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>> {
        Box::pin(async move {
            self.storage().recover_components()?;
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
            self.storage().recover_components()?;
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
            if storage.has_component_histories() {
                return component::ComponentHistory::new(&storage, &repo).uninstall(ids);
            }
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
