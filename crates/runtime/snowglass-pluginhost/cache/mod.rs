use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_bytes::ByteBuf;
use std::{
    collections::BTreeMap,
    io::{Read, Seek},
    path::{Component, Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct CacheOptions {
    roots: BTreeMap<String, PathBuf>,
}

impl CacheOptions {
    pub(crate) fn for_plugin(
        directories: &snowglass_library::Directories,
        plugin: &dyn crate::plugin::PluginInstance,
    ) -> Self {
        Self {
            roots: plugin
                .caches()
                .iter()
                .map(|cache| {
                    (
                        cache.key.clone(),
                        directories.plugin_cache(plugin.id()).join(&cache.key),
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn filesystem_caches(&self) -> Vec<crate::filesystem::NamedCache> {
        self.roots
            .iter()
            .map(|(key, path)| crate::filesystem::NamedCache {
                key: key.clone(),
                path: path.clone(),
            })
            .collect()
    }
}

struct CacheState(CacheOptions);

impl CacheState {
    fn path(&self, key: &str, relative: &str) -> Result<PathBuf, JsErrorBox> {
        let root = self.0.roots.get(key).ok_or_else(|| {
            JsErrorBox::type_error(format!("cache '{key}' is not declared by this plugin"))
        })?;
        let relative = Path::new(relative);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(JsErrorBox::type_error(
                "cache paths must contain only plain relative components",
            ));
        }
        std::fs::create_dir_all(root).map_err(JsErrorBox::from_err)?;
        let path = root.join(relative);
        let mut current = root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            if current.exists()
                && std::fs::symlink_metadata(&current)
                    .map_err(JsErrorBox::from_err)?
                    .file_type()
                    .is_symlink()
            {
                return Err(JsErrorBox::type_error(
                    "cache paths cannot traverse symbolic links",
                ));
            }
        }
        Ok(path)
    }
}

#[op2]
#[serde]
fn op_cache_keys(state: &mut OpState) -> Vec<String> {
    state
        .borrow::<CacheState>()
        .0
        .roots
        .keys()
        .cloned()
        .collect()
}

#[op2]
#[buffer]
fn op_cache_read(
    state: &mut OpState,
    #[string] key: String,
    #[string] path: String,
) -> Result<Vec<u8>, JsErrorBox> {
    let path = state.borrow::<CacheState>().path(&key, &path)?;
    std::fs::read(path).map_err(JsErrorBox::from_err)
}

#[op2(fast)]
fn op_cache_size(
    state: &mut OpState,
    #[string] key: String,
    #[string] path: String,
) -> Result<f64, JsErrorBox> {
    let path = state.borrow::<CacheState>().path(&key, &path)?;
    std::fs::metadata(path)
        .map(|metadata| metadata.len() as f64)
        .map_err(JsErrorBox::from_err)
}

#[op2]
#[buffer]
fn op_cache_read_range(
    state: &mut OpState,
    #[string] key: String,
    #[string] path: String,
    offset: f64,
    length: u32,
) -> Result<Vec<u8>, JsErrorBox> {
    let path = state.borrow::<CacheState>().path(&key, &path)?;
    let mut file = std::fs::File::open(path).map_err(JsErrorBox::from_err)?;
    if !offset.is_finite() || offset < 0.0 || offset.fract() != 0.0 || offset > u64::MAX as f64 {
        return Err(JsErrorBox::type_error("cache read offset is invalid"));
    }
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(JsErrorBox::from_err)?;
    let mut bytes = vec![0; length as usize];
    let read = file.read(&mut bytes).map_err(JsErrorBox::from_err)?;
    bytes.truncate(read);
    Ok(bytes)
}

#[op2(fast)]
fn op_cache_create(
    state: &mut OpState,
    #[string] key: String,
    #[string] path: String,
    truncate: bool,
) -> Result<(), JsErrorBox> {
    let path = state.borrow::<CacheState>().path(&key, &path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(JsErrorBox::from_err)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(truncate);
    options.open(path).map(|_| ()).map_err(JsErrorBox::from_err)
}

#[op2]
fn op_cache_write(
    state: &mut OpState,
    #[string] key: String,
    #[string] path: String,
    #[serde] bytes: ByteBuf,
    append: bool,
) -> Result<(), JsErrorBox> {
    use std::io::Write;
    let path = state.borrow::<CacheState>().path(&key, &path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(JsErrorBox::from_err)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append);
    options
        .open(path)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(JsErrorBox::from_err)
}

deno_core::extension!(snowglass_cache,
    ops = [op_cache_keys, op_cache_read, op_cache_size, op_cache_read_range, op_cache_create, op_cache_write],
    esm_entry_point = "ext:snowglass_cache/00_cache.js",
    esm = [dir "cache/js", "00_cache.js"],
    options = { options: CacheOptions },
    state = |state, options| state.put(CacheState(options.options)),
);

pub(crate) fn extension(options: CacheOptions) -> deno_core::Extension {
    snowglass_cache::init(options)
}
