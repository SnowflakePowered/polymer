use super::{ControllerId, ScanRequest, SessionId};
use crate::{
    DeviceAddr, TransportKind,
    devices::{ConnectOptions, IrCameraSetup},
};

#[derive(Debug)]
pub struct Scan {
    pub request: ScanRequest,
}

#[derive(Debug)]
pub struct StopScan {
    pub transport: TransportKind,
}

#[derive(Debug)]
pub struct Connect {
    pub address: DeviceAddr,
    pub options: ConnectOptions,
}

#[derive(Debug)]
pub struct SendRaw {
    pub session_id: SessionId,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct SetExternalOwnership {
    pub session_id: SessionId,
    pub enabled: bool,
}

#[derive(Debug)]
pub struct SetPlayerIndex {
    pub session_id: SessionId,
    pub index: u8,
}

#[derive(Debug)]
pub struct SetRumble {
    pub session_id: SessionId,
    pub low_frequency: u16,
    pub high_frequency: u16,
}

#[derive(Debug)]
pub struct SetIrCamera {
    pub session_id: SessionId,
    pub setup: IrCameraSetup,
}

#[derive(Debug)]
pub struct Sleep {
    pub session_id: SessionId,
}

#[derive(Debug)]
pub struct Disconnect {
    pub session_id: SessionId,
}

#[derive(Debug)]
pub struct Forget {
    pub controller_id: ControllerId,
}
