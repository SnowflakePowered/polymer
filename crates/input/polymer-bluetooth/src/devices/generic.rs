use async_trait::async_trait;
use uuid::Uuid;

use super::{ControllerDevice, DeviceIdentity, MatchConfidence, ProfileId};

const HID_SERVICE: Uuid = Uuid::from_u128(0x0000_1124_0000_1000_8000_0080_5f9b_34fb);
const BLE_HID_SERVICE: Uuid = Uuid::from_u128(0x0000_1812_0000_1000_8000_0080_5f9b_34fb);

#[derive(Default)]
pub struct GenericHidController;

#[async_trait]
impl ControllerDevice for GenericHidController {
    fn id(&self) -> ProfileId {
        ProfileId::new("generic.hid")
    }

    fn display_name(&self) -> &'static str {
        "Generic Bluetooth HID controller"
    }

    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        (identity.has_service(HID_SERVICE)
            || identity.has_service(BLE_HID_SERVICE)
            || identity
                .class_of_device
                .is_some_and(|class| class & 0x1f00 == 0x0500))
        .then_some(MatchConfidence::Generic)
    }
}
