use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    CableDeviceIdentity, CablePairingHost, CablePairingSession, CablePairingState,
    VerifiedCablePairing,
};
use crate::{Error, Result};

pub(super) struct FeatureSession<P> {
    identity: CableDeviceIdentity,
    protocol: Arc<Mutex<P>>,
}

impl<P> FeatureSession<P> {
    pub fn new(identity: CableDeviceIdentity, protocol: P) -> Self {
        Self {
            identity,
            protocol: Arc::new(Mutex::new(protocol)),
        }
    }
}

#[async_trait]
impl<P: FeaturePairing + Send + 'static> CablePairingSession for FeatureSession<P> {
    fn identity(&self) -> &CableDeviceIdentity {
        &self.identity
    }

    async fn read_pairing(&mut self) -> Result<CablePairingState> {
        let protocol = self.protocol.clone();
        tokio::task::spawn_blocking(move || {
            protocol
                .lock()
                .map_err(|_| Error::ChannelClosed)?
                .read_pairing()
        })
        .await
        .map_err(|error| Error::Platform(format!("USB pairing read task failed: {error}")))?
    }

    async fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        let host = *host;
        let protocol = self.protocol.clone();
        tokio::task::spawn_blocking(move || {
            protocol
                .lock()
                .map_err(|_| Error::ChannelClosed)?
                .pair(&host)
        })
        .await
        .map_err(|error| Error::Platform(format!("USB pairing task failed: {error}")))?
    }
}

/// Synchronous USB feature protocol, executed on the blocking pool.
pub(super) trait FeaturePairing {
    fn read_pairing(&mut self) -> Result<CablePairingState>;
    fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing>;
}
