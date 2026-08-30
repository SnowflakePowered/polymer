use serde::Deserialize;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PipeEnvironment {
    port: u8,
    product_id: u16,
    pipe: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SyntheticWiimote {
    pub(crate) path: Vec<u16>,
    pub(crate) pipe: Vec<u16>,
    pub(crate) product_id: u16,
}

impl SyntheticWiimote {
    pub(crate) fn load() -> Vec<Self> {
        let Some(json) = snowglass_hook::config::bluetooth_hid_pipes() else {
            return Vec::new();
        };
        serde_json::from_str::<Vec<PipeEnvironment>>(&json)
            .unwrap_or_else(|error| {
                tracing::error!(%error, "invalid Bluetooth HID pipe configuration");
                Vec::new()
            })
            .into_iter()
            .map(|pipe| {
                let path = format!(
                    r"\\?\hid#vid_057e&pid_{:04x}#snowglass-{}#{{4d1e55b2-f16f-11cf-88cb-001111000030}}",
                    pipe.product_id, pipe.port,
                );
                Self {
                    path: OsStr::new(&path).encode_wide().chain(Some(0)).collect(),
                    pipe: OsStr::new(&pipe.pipe).encode_wide().chain(Some(0)).collect(),
                    product_id: pipe.product_id,
                }
            })
            .collect()
    }

    pub(crate) fn path_matches(&self, path: &[u16]) -> bool {
        self.path[..self.path.len().saturating_sub(1)].eq_ignore_ascii_case(path)
    }
}

trait WideAsciiExt {
    fn eq_ignore_ascii_case(&self, other: &[u16]) -> bool;
}

impl WideAsciiExt for [u16] {
    fn eq_ignore_ascii_case(&self, other: &[u16]) -> bool {
        self.len() == other.len()
            && self.iter().zip(other).all(|(left, right)| {
                u8::try_from(*left)
                    .ok()
                    .zip(u8::try_from(*right).ok())
                    .is_some_and(|(left, right)| left.eq_ignore_ascii_case(&right))
            })
    }
}
