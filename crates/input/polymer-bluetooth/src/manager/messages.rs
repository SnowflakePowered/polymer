use super::{ControllerId, ScanRequest, SessionId};
use crate::{
    DeviceAddr, TransportKind,
    devices::{ConnectOptions, IrCameraSetup},
};

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct Scan {
    pub request: ScanRequest,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct StopScan {
    pub transport: TransportKind,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct Connect {
    pub address: DeviceAddr,
    pub options: ConnectOptions,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct SendRaw {
    pub session_id: SessionId,
    pub bytes: Vec<u8>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct SetExternalOwnership {
    pub session_id: SessionId,
    pub enabled: bool,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct SetPlayerIndex {
    pub session_id: SessionId,
    pub index: u8,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct SetRumble {
    pub session_id: SessionId,
    pub low_frequency: u16,
    pub high_frequency: u16,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct SetIrCamera {
    pub session_id: SessionId,
    pub setup: IrCameraSetup,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct Sleep {
    pub session_id: SessionId,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct Disconnect {
    pub session_id: SessionId,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct Forget {
    pub controller_id: ControllerId,
}

/// Disconnect controllers and stop the manager after hardware cleanup completes.
#[derive(Debug, actix::Message)]
#[rtype(result = "crate::Result<()>")]
pub struct Shutdown;
