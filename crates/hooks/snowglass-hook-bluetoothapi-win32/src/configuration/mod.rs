use snowglass_input_layout::topology::RawHidBinding;
use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

#[derive(Clone, Debug)]
pub(crate) struct SyntheticWiimote {
    pub(crate) path: Vec<u16>,
    pub(crate) pipe: Vec<u16>,
    pub(crate) product_id: u16,
}

impl SyntheticWiimote {
    pub(crate) fn from_binding(binding: &RawHidBinding) -> Self {
        let path = format!(
            r"\\?\hid#vid_{:04x}&pid_{:04x}#snowglass-{}#{{4d1e55b2-f16f-11cf-88cb-001111000030}}",
            binding.vendor_id, binding.product_id, binding.port,
        );
        Self {
            path: OsStr::new(&path).encode_wide().chain(Some(0)).collect(),
            pipe: OsStr::new(&binding.pipe)
                .encode_wide()
                .chain(Some(0))
                .collect(),
            product_id: binding.product_id,
        }
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
