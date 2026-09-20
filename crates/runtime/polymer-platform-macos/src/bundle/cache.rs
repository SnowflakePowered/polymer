use super::StagedBundle;
use std::{
    fs::{self, File},
    io,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
};

pub(super) struct BundleCache;

impl BundleCache {
    // Bump when the entitlement or signing policy changes.
    const POLICY: &[u8] = b"polymer-signed-bundle-v1";

    pub(super) fn prepare(source: &Path, root: &Path) -> io::Result<StagedBundle> {
        let source = source.canonicalize()?;
        fs::create_dir_all(root)?;
        // Serialize publication across processes, including concurrent GUI launches.
        // The OS releases the lock if preparation fails or the process exits.
        let lock = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        lock.lock()?;

        let fingerprint = Self::fingerprint(&source)?;
        let mut key = blake3::Hasher::new();
        key.update(Self::POLICY);
        key.update(source.as_os_str().as_encoded_bytes());
        key.update(fingerprint.as_bytes());
        let entry = root.join(key.finalize().to_hex().as_str());
        let name = source
            .file_name()
            .ok_or_else(|| io::Error::other("bundle has no name"))?;
        let bundle = entry.join(name);
        if entry.exists() {
            return StagedBundle::open(bundle);
        }

        let directory = tempfile::Builder::new()
            .prefix(".preparing-")
            .tempdir_in(root)?;
        let staged = directory.path().join(name);
        StagedBundle::run(
            Command::new("/bin/cp")
                .arg("-Rcp")
                .arg(&source)
                .arg(&staged),
        )?;
        if Self::fingerprint(&staged)? != fingerprint {
            return Err(io::Error::other(
                "emulator bundle changed while being staged; retry launch",
            ));
        }
        StagedBundle::sign(&staged, directory.path())?;
        StagedBundle::open(staged)?;
        // Only complete, verified bundles become visible as cache entries. Old
        // versions remain available to guests that are still running from them.
        fs::rename(directory.path(), &entry)?;
        StagedBundle::open(bundle)
    }

    fn fingerprint(root: &Path) -> io::Result<blake3::Hash> {
        let mut hash = blake3::Hasher::new();
        let mut pending = vec![root.to_owned()];
        while let Some(path) = pending.pop() {
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let bytes = relative.as_os_str().as_encoded_bytes();
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
            let metadata = fs::symlink_metadata(&path)?;
            hash.update(&metadata.permissions().mode().to_le_bytes());
            if metadata.is_dir() {
                let mut children = fs::read_dir(&path)?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<io::Result<Vec<_>>>()?;
                children.sort();
                pending.extend(children);
            } else if metadata.is_symlink() {
                let target = fs::read_link(&path)?;
                let bytes = target.as_os_str().as_encoded_bytes();
                hash.update(&(bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            } else if metadata.is_file() {
                let mut contents = blake3::Hasher::new();
                contents.update_reader(File::open(&path)?)?;
                hash.update(contents.finalize().as_bytes());
            } else {
                return Err(io::Error::other("unsupported file type in emulator bundle"));
            }
        }
        Ok(hash.finalize())
    }
}
