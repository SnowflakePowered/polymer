use bytes::{BufMut, Bytes, BytesMut};

use crate::DeviceAddr;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Opcode(pub u16);

impl Opcode {
    pub const INQUIRY: Self = Self::new(0x01, 0x0001);
    pub const INQUIRY_CANCEL: Self = Self::new(0x01, 0x0002);
    pub const CREATE_CONNECTION: Self = Self::new(0x01, 0x0005);
    pub const ACCEPT_CONNECTION_REQUEST: Self = Self::new(0x01, 0x0009);
    pub const REJECT_CONNECTION_REQUEST: Self = Self::new(0x01, 0x000a);
    pub const DISCONNECT: Self = Self::new(0x01, 0x0006);
    pub const LINK_KEY_REQUEST_REPLY: Self = Self::new(0x01, 0x000b);
    pub const LINK_KEY_REQUEST_NEGATIVE_REPLY: Self = Self::new(0x01, 0x000c);
    pub const PIN_CODE_REQUEST_REPLY: Self = Self::new(0x01, 0x000d);
    pub const PIN_CODE_REQUEST_NEGATIVE_REPLY: Self = Self::new(0x01, 0x000e);
    pub const AUTHENTICATION_REQUESTED: Self = Self::new(0x01, 0x0011);
    pub const SET_CONNECTION_ENCRYPTION: Self = Self::new(0x01, 0x0013);
    pub const READ_REMOTE_SUPPORTED_FEATURES: Self = Self::new(0x01, 0x001b);
    pub const READ_REMOTE_EXTENDED_FEATURES: Self = Self::new(0x01, 0x001c);
    pub const REMOTE_NAME_REQUEST: Self = Self::new(0x01, 0x0019);
    pub const IO_CAPABILITY_REQUEST_REPLY: Self = Self::new(0x01, 0x002b);
    pub const USER_CONFIRMATION_REQUEST_REPLY: Self = Self::new(0x01, 0x002c);

    pub const ROLE_DISCOVERY: Self = Self::new(0x02, 0x0009);
    pub const SWITCH_ROLE: Self = Self::new(0x02, 0x000b);
    pub const WRITE_DEFAULT_LINK_POLICY_SETTINGS: Self = Self::new(0x02, 0x000f);

    pub const SET_EVENT_MASK: Self = Self::new(0x03, 0x0001);
    pub const RESET: Self = Self::new(0x03, 0x0003);
    pub const SET_EVENT_FILTER: Self = Self::new(0x03, 0x0005);
    pub const WRITE_LOCAL_NAME: Self = Self::new(0x03, 0x0013);
    pub const WRITE_CONNECTION_ACCEPT_TIMEOUT: Self = Self::new(0x03, 0x0016);
    pub const WRITE_PAGE_TIMEOUT: Self = Self::new(0x03, 0x0018);
    pub const WRITE_SCAN_ENABLE: Self = Self::new(0x03, 0x001a);
    pub const WRITE_PAGE_SCAN_ACTIVITY: Self = Self::new(0x03, 0x001c);
    pub const WRITE_AUTHENTICATION_ENABLE: Self = Self::new(0x03, 0x0020);
    pub const WRITE_CLASS_OF_DEVICE: Self = Self::new(0x03, 0x0024);
    pub const WRITE_AUTOMATIC_FLUSH_TIMEOUT: Self = Self::new(0x03, 0x0028);
    pub const WRITE_INQUIRY_MODE: Self = Self::new(0x03, 0x0045);
    pub const WRITE_PAGE_SCAN_TYPE: Self = Self::new(0x03, 0x0047);
    pub const WRITE_SIMPLE_PAIRING_MODE: Self = Self::new(0x03, 0x0056);

    pub const READ_LOCAL_SUPPORTED_FEATURES: Self = Self::new(0x04, 0x0003);
    pub const READ_LOCAL_VERSION_INFORMATION: Self = Self::new(0x04, 0x0001);
    pub const READ_BUFFER_SIZE: Self = Self::new(0x04, 0x0005);
    pub const READ_BD_ADDR: Self = Self::new(0x04, 0x0009);

    pub const LE_READ_BUFFER_SIZE: Self = Self::new(0x08, 0x0002);

    pub const fn new(ogf: u16, ocf: u16) -> Self {
        Self((ogf << 10) | (ocf & 0x03ff))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    opcode: Opcode,
    parameters: Bytes,
}

impl Command {
    pub fn new(opcode: Opcode, parameters: impl Into<Bytes>) -> Self {
        Self {
            opcode,
            parameters: parameters.into(),
        }
    }

    pub fn reset() -> Self {
        Self::new(Opcode::RESET, Bytes::new())
    }

    pub fn inquiry(duration_units: u8, max_responses: u8) -> Self {
        Self::inquiry_with_lap([0x33, 0x8b, 0x9e], duration_units, max_responses)
    }

    /// Uses the Limited Dedicated Inquiry Access Code used by a Wii console.
    /// Some third-party Wii Remotes do not answer GIAC inquiry.
    pub fn limited_inquiry(duration_units: u8, max_responses: u8) -> Self {
        Self::inquiry_with_lap([0x00, 0x8b, 0x9e], duration_units, max_responses)
    }

    fn inquiry_with_lap(lap: [u8; 3], duration_units: u8, max_responses: u8) -> Self {
        Self::new(
            Opcode::INQUIRY,
            Bytes::from(vec![lap[0], lap[1], lap[2], duration_units, max_responses]),
        )
    }

    pub fn inquiry_cancel() -> Self {
        Self::new(Opcode::INQUIRY_CANCEL, Bytes::new())
    }

    pub fn create_connection(addr: DeviceAddr, packet_type: u16, clock_offset: u16) -> Self {
        let mut p = BytesMut::with_capacity(13);
        p.extend_from_slice(&addr.hci_bytes());
        p.put_u16_le(packet_type);
        p.put_u8(0x01); // page scan repetition mode R1
        p.put_u8(0x00); // reserved
        p.put_u16_le(clock_offset);
        p.put_u8(0x01); // allow role switch
        Self::new(Opcode::CREATE_CONNECTION, p.freeze())
    }

    pub fn accept_connection(addr: DeviceAddr, remain_master: bool) -> Self {
        let mut p = BytesMut::with_capacity(7);
        p.extend_from_slice(&addr.hci_bytes());
        p.put_u8(u8::from(!remain_master));
        Self::new(Opcode::ACCEPT_CONNECTION_REQUEST, p.freeze())
    }

    pub fn reject_connection(addr: DeviceAddr, reason: u8) -> Self {
        let mut p = BytesMut::with_capacity(7);
        p.extend_from_slice(&addr.hci_bytes());
        p.put_u8(reason);
        Self::new(Opcode::REJECT_CONNECTION_REQUEST, p.freeze())
    }

    pub fn disconnect(handle: u16, reason: u8) -> Self {
        let mut p = BytesMut::with_capacity(3);
        p.put_u16_le(handle & 0x0fff);
        p.put_u8(reason);
        Self::new(Opcode::DISCONNECT, p.freeze())
    }

    pub fn handle_only(opcode: Opcode, handle: u16) -> Self {
        Self::new(
            opcode,
            Bytes::copy_from_slice(&(handle & 0x0fff).to_le_bytes()),
        )
    }

    pub fn set_connection_encryption(handle: u16, enabled: bool) -> Self {
        let mut p = BytesMut::with_capacity(3);
        p.put_u16_le(handle & 0x0fff);
        p.put_u8(u8::from(enabled));
        Self::new(Opcode::SET_CONNECTION_ENCRYPTION, p.freeze())
    }

    pub fn role_discovery(handle: u16) -> Self {
        Self::handle_only(Opcode::ROLE_DISCOVERY, handle)
    }

    pub fn switch_role(addr: DeviceAddr, become_master: bool) -> Self {
        let mut p = BytesMut::with_capacity(7);
        p.extend_from_slice(&addr.hci_bytes());
        p.put_u8(u8::from(!become_master));
        Self::new(Opcode::SWITCH_ROLE, p.freeze())
    }

    pub fn remote_name_request(addr: DeviceAddr, clock_offset: u16) -> Self {
        let mut p = BytesMut::with_capacity(10);
        p.extend_from_slice(&addr.hci_bytes());
        p.put_u8(0x01);
        p.put_u8(0x00);
        p.put_u16_le(clock_offset);
        Self::new(Opcode::REMOTE_NAME_REQUEST, p.freeze())
    }

    pub fn read_remote_extended_features(handle: u16, page: u8) -> Self {
        let mut p = BytesMut::with_capacity(3);
        p.put_u16_le(handle & 0x0fff);
        p.put_u8(page);
        Self::new(Opcode::READ_REMOTE_EXTENDED_FEATURES, p.freeze())
    }

    pub fn io_capability_reply(addr: DeviceAddr) -> Self {
        let mut p = BytesMut::with_capacity(9);
        p.extend_from_slice(&addr.hci_bytes());
        p.put_u8(0x03); // NoInputNoOutput
        p.put_u8(0x00); // no OOB authentication data
        // Match Nintendo's known-good BlueRetro SSP exchange. The host still
        // persists the notified link key itself; this field describes the
        // controller-side SSP ceremony, not our key-store policy.
        p.put_u8(0x00); // no MITM, no bonding
        Self::new(Opcode::IO_CAPABILITY_REQUEST_REPLY, p.freeze())
    }

    pub fn pin_reply(addr: DeviceAddr, pin: &[u8]) -> Option<Self> {
        if pin.len() > 16 {
            return None;
        }
        let mut p = BytesMut::zeroed(23);
        p[..6].copy_from_slice(&addr.hci_bytes());
        p[6] = pin.len() as u8;
        p[7..7 + pin.len()].copy_from_slice(pin);
        Some(Self::new(Opcode::PIN_CODE_REQUEST_REPLY, p.freeze()))
    }

    pub fn link_key_reply(addr: DeviceAddr, key: [u8; 16]) -> Self {
        let mut p = BytesMut::with_capacity(22);
        p.extend_from_slice(&addr.hci_bytes());
        p.extend_from_slice(&key);
        Self::new(Opcode::LINK_KEY_REQUEST_REPLY, p.freeze())
    }

    pub fn address_only(opcode: Opcode, addr: DeviceAddr) -> Self {
        Self::new(opcode, Bytes::copy_from_slice(&addr.hci_bytes()))
    }

    pub fn write_scan_enable(inquiry_scan: bool, page_scan: bool) -> Self {
        Self::new(
            Opcode::WRITE_SCAN_ENABLE,
            Bytes::from(vec![u8::from(inquiry_scan) | (u8::from(page_scan) << 1)]),
        )
    }

    pub fn write_connection_accept_timeout(slots: u16) -> Self {
        Self::new(
            Opcode::WRITE_CONNECTION_ACCEPT_TIMEOUT,
            Bytes::copy_from_slice(&slots.to_le_bytes()),
        )
    }

    pub fn write_page_timeout(slots: u16) -> Self {
        Self::new(
            Opcode::WRITE_PAGE_TIMEOUT,
            Bytes::copy_from_slice(&slots.to_le_bytes()),
        )
    }

    pub fn write_page_scan_activity(interval: u16, window: u16) -> Self {
        let mut parameters = BytesMut::with_capacity(4);
        parameters.put_u16_le(interval);
        parameters.put_u16_le(window);
        Self::new(Opcode::WRITE_PAGE_SCAN_ACTIVITY, parameters.freeze())
    }

    pub fn write_page_scan_type(interlaced: bool) -> Self {
        Self::new(
            Opcode::WRITE_PAGE_SCAN_TYPE,
            Bytes::from(vec![u8::from(interlaced)]),
        )
    }

    pub fn write_authentication_enable(enabled: bool) -> Self {
        Self::new(
            Opcode::WRITE_AUTHENTICATION_ENABLE,
            Bytes::from(vec![u8::from(enabled)]),
        )
    }

    pub fn write_local_name(name: &str) -> Self {
        let mut parameters = BytesMut::zeroed(248);
        let bytes = name.as_bytes();
        let length = bytes.len().min(parameters.len() - 1);
        parameters[..length].copy_from_slice(&bytes[..length]);
        Self::new(Opcode::WRITE_LOCAL_NAME, parameters.freeze())
    }

    pub fn write_class_of_device(class: [u8; 3]) -> Self {
        Self::new(
            Opcode::WRITE_CLASS_OF_DEVICE,
            Bytes::copy_from_slice(&class),
        )
    }

    pub fn write_default_link_policy(settings: u16) -> Self {
        Self::new(
            Opcode::WRITE_DEFAULT_LINK_POLICY_SETTINGS,
            Bytes::copy_from_slice(&settings.to_le_bytes()),
        )
    }

    pub fn write_automatic_flush_timeout(handle: u16, slots: u16) -> Self {
        let mut p = BytesMut::with_capacity(4);
        p.put_u16_le(handle & 0x0fff);
        p.put_u16_le(slots);
        Self::new(Opcode::WRITE_AUTOMATIC_FLUSH_TIMEOUT, p.freeze())
    }

    pub fn set_event_mask(mask: u64) -> Self {
        Self::new(
            Opcode::SET_EVENT_MASK,
            Bytes::copy_from_slice(&mask.to_le_bytes()),
        )
    }

    /// Selects the event format returned by inquiry. Standard mode produces
    /// Inquiry Result (0x02); RSSI and extended modes use 0x22 and 0x2f.
    pub fn write_inquiry_mode(mode: u8) -> Self {
        Self::new(Opcode::WRITE_INQUIRY_MODE, Bytes::from(vec![mode]))
    }

    pub fn write_simple_pairing_mode(enabled: bool) -> Self {
        Self::new(
            Opcode::WRITE_SIMPLE_PAIRING_MODE,
            Bytes::from(vec![u8::from(enabled)]),
        )
    }

    /// Limits inquiry results to the Peripheral major device class.
    pub fn set_inquiry_peripheral_filter() -> Self {
        Self::new(
            Opcode::SET_EVENT_FILTER,
            Bytes::from_static(&[
                0x01, // inquiry result filter
                0x01, // class-of-device condition
                0x00, 0x05, 0x00, 0x00, 0x1f, 0x00,
            ]),
        )
    }

    /// Clears controller-side event filtering. Some Wii Remote clones expose
    /// an incorrect Class of Device, so discovery policy belongs in userspace.
    pub fn clear_event_filters() -> Self {
        Self::new(Opcode::SET_EVENT_FILTER, Bytes::from_static(&[0x00]))
    }

    pub const fn opcode(&self) -> Opcode {
        self.opcode
    }
    pub fn parameters(&self) -> &[u8] {
        &self.parameters
    }

    pub fn encode(&self) -> Bytes {
        assert!(
            self.parameters.len() <= u8::MAX as usize,
            "HCI command is too large"
        );
        let mut packet = BytesMut::with_capacity(3 + self.parameters.len());
        packet.put_u16_le(self.opcode.0);
        packet.put_u8(self.parameters.len() as u8);
        packet.extend_from_slice(&self.parameters);
        packet.freeze()
    }
}
