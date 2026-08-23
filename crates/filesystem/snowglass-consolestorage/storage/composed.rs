use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use futures::future::select_all;
use tokio::io::AsyncRead;

use crate::{
    BoxFuture, ChangeSignal, DirEntry, Error, InstallResult, Metadata, MountableStorage, ReadDir,
    Storage, StorageFile, StorageRead, StorageStream,
};

/// A fixed-lifetime, ordered composition. The first storage that claims a path
/// owns it. Directory listings are unioned, but entries are exposed only when
/// they exist in their winning storage.
pub struct ComposedStorage {
    layers: Box<[Arc<dyn MountableStorage>]>,
}

impl ComposedStorage {
    pub fn new(layers: Vec<Arc<dyn MountableStorage>>) -> Result<Self, Error> {
        if layers.is_empty() {
            return Err(Error::Unsupported(
                "a storage composition requires at least one layer".to_owned(),
            ));
        }
        Ok(Self {
            layers: layers.into_boxed_slice(),
        })
    }

    fn owner(&self, path: &str) -> Result<&Arc<dyn MountableStorage>, Error> {
        self.layers
            .iter()
            .find(|storage| storage.claims(path))
            .ok_or_else(|| Error::Unsupported(format!("no storage claims `{path}`")))
    }

    fn is_not_found(error: &Error) -> bool {
        matches!(
            error,
            Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
        )
    }

    fn child(parent: &str, name: &str) -> String {
        if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}/{name}")
        }
    }

    fn surface(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let mut candidates = BTreeMap::<String, ()>::new();
        for layer in &self.layers {
            match layer.read_dir(path, false) {
                Ok(entries) => {
                    for entry in entries {
                        candidates.insert(entry.name, ());
                    }
                }
                Err(error) if Self::is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }

        let mut entries = Vec::with_capacity(candidates.len());
        for name in candidates.into_keys() {
            let child = Self::child(path, &name);
            let owner = self.owner(&child)?;
            match owner.metadata(&child) {
                Ok(metadata) => entries.push(DirEntry {
                    name,
                    path: child,
                    metadata,
                }),
                Err(error) if Self::is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(entries)
    }
}

impl fmt::Debug for ComposedStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComposedStorage")
            .field("layers", &self.layers)
            .finish()
    }
}

impl Storage for ComposedStorage {
    fn exists(&self, path: &str) -> bool {
        self.owner(path).is_ok_and(|storage| storage.exists(path))
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.owner(path)?.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if recursive {
            return ReadDir::recursive(self, path);
        }
        Ok(ReadDir::surface(self.surface(path)?.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        self.owner(path)?.create(path)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        self.owner(path)?.open(path, write)
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        self.owner(path)?.open_read(path)
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        self.owner(path)?.append(path)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.remove(path)
    }

    fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        let mut grouped = HashMap::<usize, (Arc<dyn MountableStorage>, Vec<&str>)>::new();
        for path in paths {
            let owner = self.owner(path)?.clone();
            grouped
                .entry(Arc::as_ptr(&owner) as *const () as usize)
                .or_insert_with(|| (owner, Vec::new()))
                .1
                .push(path);
        }

        let mut signals = Vec::new();
        for (_, (storage, paths)) in grouped {
            if let Some(signal) = storage.watch(&paths)? {
                signals.push(signal);
            }
        }
        match signals.len() {
            0 => Ok(None),
            1 => Ok(signals.pop()),
            _ => Ok(Some(Box::new(ComposedSignal { signals }))),
        }
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        match self.owner("") {
            Ok(storage) => storage.label(),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        match self.owner("") {
            Ok(storage) => storage.set_label(label),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        match self.owner(dest) {
            Ok(storage) => storage.install_file(source, dest),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

impl MountableStorage for ComposedStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let from_owner = self.owner(from)?;
        let to_owner = self.owner(to)?;
        if !Arc::ptr_eq(from_owner, to_owner) {
            return Err(Error::Unsupported(
                "rename crosses composed storage layers".to_owned(),
            ));
        }
        from_owner.rename(from, to)
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.owner(path)?.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.sync(path)
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.finish(path)
    }
}

struct ComposedSignal {
    signals: Vec<Box<dyn ChangeSignal>>,
}

impl ChangeSignal for ComposedSignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let pending = self
                .signals
                .iter()
                .map(|signal| signal.changed())
                .collect::<Vec<_>>();
            let _ = select_all(pending).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DirectoryStorage;
    use crate::impls::wiiu::nand::WiiUNand;
    use crate::nand::VirtualNand;
    use std::io::{Read, Write};

    async fn wiiu() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        WiiUNand,
        Arc<dyn crate::SaveRepo>,
    ) {
        let save_root = tempfile::tempdir().unwrap();
        let nand_root = tempfile::tempdir().unwrap();
        let nand = WiiUNand::new_empty(
            DirectoryStorage::new(nand_root.path()).await.unwrap(),
            uuid::Uuid::now_v7(),
            None,
        )
        .await
        .unwrap();
        let saves = WiiUNand::new_save_repo(
            DirectoryStorage::new(save_root.path()).await.unwrap(),
            uuid::Uuid::now_v7(),
            None,
        )
        .await
        .unwrap();
        (save_root, nand_root, nand, saves)
    }

    #[tokio::test]
    async fn first_claimant_owns_missing_paths_and_directories_are_unioned() {
        let (save_root, _nand_root, nand, saves) = wiiu().await;
        nand.root().create_dir_all("usr/save/game").unwrap();
        nand.root()
            .create_dir_all("usr/title/game/content")
            .unwrap();
        nand.root()
            .create("usr/save/game/stale.bin")
            .unwrap()
            .write_all(b"stale")
            .unwrap();
        nand.root()
            .create("usr/title/game/content/app.bin")
            .unwrap()
            .write_all(b"game")
            .unwrap();

        let composed = nand.compose(saves).unwrap();

        assert!(!composed.exists("usr/save/game/stale.bin"));
        assert!(composed.exists("usr/title/game/content/app.bin"));

        composed.create_dir_all("usr/save/game").unwrap();
        composed
            .create("usr/save/game/new.bin")
            .unwrap()
            .write_all(b"save")
            .unwrap();
        assert!(save_root.path().join("usr/save/game/new.bin").exists());
        assert!(!nand.root().exists("usr/save/game/new.bin"));

        let usr = composed
            .read_dir("usr", false)
            .unwrap()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(usr, ["save", "title"]);

        let mut game = Vec::new();
        composed
            .open_read("usr/title/game/content/app.bin")
            .unwrap()
            .read_to_end(&mut game)
            .unwrap();
        assert_eq!(game, b"game");
    }

    #[tokio::test]
    async fn rejects_cross_layer_renames() {
        let (_save_root, _nand_root, nand, saves) = wiiu().await;
        let composed = nand.compose(saves).unwrap();

        assert!(matches!(
            composed.rename("usr/save/a", "usr/title/a"),
            Err(Error::Unsupported(_))
        ));
    }
}
