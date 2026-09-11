//! L2CAP support for Bluetooth HID control and interrupt channels.

mod link;
mod packet;
mod signaling;

pub use link::{Channel, ChannelSender, Link};
pub use packet::{Packet, SIGNALING_CID};
pub use signaling::{ConfigurationOption, QualityOfService, Signal};

const DEFAULT_MTU: u16 = 672;

pub const HID_CONTROL_PSM: u16 = 0x0011;
pub const HID_INTERRUPT_PSM: u16 = 0x0013;
pub const SDP_PSM: u16 = 0x0001;
