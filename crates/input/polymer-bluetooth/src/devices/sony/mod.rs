//! Sony controller identities and lifecycle behavior.

use async_trait::async_trait;

use super::{ControllerDevice, DeviceIdentity, MatchConfidence, ProfileId};

const SONY_VENDOR_ID: u16 = 0x054c;

pub mod dualshock3;
pub use dualshock3::DualShock3;
pub mod dualsense;
pub use dualsense::DualSense;

pub mod dualshock4;
pub use dualshock4::DualShock4;
pub mod motion;
pub mod protocol;

#[derive(Default)]
pub struct UnknownSonyController;

#[async_trait]
impl ControllerDevice for UnknownSonyController {
    fn id(&self) -> ProfileId {
        ProfileId::new("sony.hid")
    }

    fn display_name(&self) -> &'static str {
        "Sony HID controller"
    }

    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        identity
            .pnp
            .as_ref()
            .is_some_and(|pnp| pnp.vendor_id == SONY_VENDOR_ID)
            .then_some(MatchConfidence::Family)
    }
}
