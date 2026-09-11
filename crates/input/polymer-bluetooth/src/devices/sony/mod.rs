//! Sony controller identities and lifecycle behavior.

use async_trait::async_trait;

use super::{ControllerDevice, DeviceIdentity, MatchConfidence, ProfileId};

const SONY_VENDOR_ID: u16 = 0x054c;

mod dualshock3;
pub use dualshock3::DualShock3;

macro_rules! sony_controller {
    ($controller:ident, $id:literal, $name:literal, [$($product:literal),+ $(,)?]) => {
        #[derive(Default)]
        pub struct $controller;

        #[async_trait]
        impl ControllerDevice for $controller {
            fn id(&self) -> ProfileId {
                ProfileId::new($id)
            }

            fn display_name(&self) -> &'static str {
                $name
            }

            fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
                identity
                    .has_pnp(SONY_VENDOR_ID, &[$($product),+])
                    .then_some(MatchConfidence::Pnp)
            }
        }
    };
}

sony_controller!(
    DualShock4,
    "sony.dualshock4",
    "DualShock 4",
    [0x05c4, 0x09cc, 0x0ba0]
);
sony_controller!(DualSense, "sony.dualsense", "DualSense", [0x0ce6, 0x0df2]);

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
