use snowglass_input_layout::topology::RawHidBinding;
use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

#[derive(Clone, Debug)]
pub(crate) struct SyntheticWiimote {
    pub(crate) path: Vec<u16>,
    pub(crate) pipe: Vec<u16>,
    pub(crate) product_id: u16,
    pub(crate) active: bool,
}

impl SyntheticWiimote {
    pub(crate) fn for_port(session: &str, port: u8, binding: Option<&RawHidBinding>) -> Self {
        // Keep the enumerated identity stable across live topology changes.
        // Dolphin caches this path until Windows broadcasts a PnP notification;
        // HidD_GetAttributes supplies the active remote's actual PID separately.
        let path = format!(
            r"\\?\hid#vid_057e&pid_0306#snowglass-{port}#{{4d1e55b2-f16f-11cf-88cb-001111000030}}",
        );
        let pipe = binding.map_or_else(
            || format!(r"\\.\pipe\{session}-hid-{port}"),
            |binding| binding.pipe.clone(),
        );
        Self {
            path: OsStr::new(&path).encode_wide().chain(Some(0)).collect(),
            pipe: OsStr::new(&pipe).encode_wide().chain(Some(0)).collect(),
            product_id: binding.map_or(0x0306, |binding| binding.product_id),
            active: binding.is_some(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_path_is_stable_while_the_active_binding_changes() {
        let inactive = SyntheticWiimote::for_port("test-session", 2, None);
        let binding = RawHidBinding {
            player: 2,
            port: 2,
            vendor_id: 0x057e,
            product_id: 0x0330,
            pipe: r"\\.\pipe\test-session-hid-2".to_owned(),
        };
        let active = SyntheticWiimote::for_port("test-session", 2, Some(&binding));

        assert_eq!(inactive.path, active.path);
        assert!(!inactive.active);
        assert!(active.active);
        assert_eq!(active.product_id, 0x0330);
    }
}
