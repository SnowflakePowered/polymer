use std::sync::Arc;

use super::{DepotFile, InstallContext, Installable, artifacts::Artifact};
use crate::{ArtifactComponent, DirectoryStorage, Error, VolumeStorage};

pub(crate) struct ComponentStage {
    pub storage: Arc<DirectoryStorage>,
    temporary: tempfile::TempDir,
}

impl ComponentStage {
    pub fn new() -> Result<Self, Error> {
        let temporary =
            tempfile::tempdir().map_err(|error| Error::io(&std::env::temp_dir(), error))?;
        Ok(Self {
            storage: Arc::new(DirectoryStorage::unmanaged(temporary.path())),
            temporary,
        })
    }

    pub fn install<I: Installable>(
        self,
        inner: I,
        component: ArtifactComponent,
        metadata: serde_json::Value,
    ) -> impl Installable {
        ComponentInstall {
            inner,
            component,
            metadata,
            stage: self,
        }
    }
}

struct ComponentInstall<I> {
    inner: I,
    component: ArtifactComponent,
    metadata: serde_json::Value,
    stage: ComponentStage,
}

impl<I: Installable> Installable for ComponentInstall<I> {
    async fn next_batch(
        &mut self,
        context: &InstallContext,
    ) -> Result<Option<Vec<DepotFile>>, Error> {
        self.inner.next_batch(context).await
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        self.inner.finish(manifest).await
    }

    async fn finish_and_record(
        self,
        manifest: Vec<DepotFile>,
        storage: Arc<dyn VolumeStorage>,
        context: &InstallContext,
    ) -> Result<Artifact, Error> {
        let artifact = self.inner.finish(manifest).await?;
        storage
            .artifacts()
            .install_component(
                &self.component,
                &self.metadata,
                &artifact,
                self.stage.storage,
                context,
            )
            .await?;
        drop(self.stage.temporary);
        storage
            .artifacts()
            .artifact(artifact.id())
            .await?
            .ok_or_else(|| Error::not_found(artifact.id()))
    }
}
