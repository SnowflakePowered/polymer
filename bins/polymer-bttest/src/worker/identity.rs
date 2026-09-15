use super::*;
use polymer_bluetooth::{
    devices::ConnectionPolicy,
    transport::bredr::{
        l2cap::Link,
        sdp::{self, SdpIdentity},
    },
};

impl BrIdentity {
    fn device_identity(address: DeviceAddr) -> DeviceIdentity {
        DeviceIdentity {
            address,
            transport: TransportKind::BrEdr,
            name: None,
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        }
    }

    pub(super) fn policy(&self, address: DeviceAddr, slot: usize) -> ConnectionPolicy {
        self.controller()
            .map_or_else(ConnectionPolicy::default, |controller| {
                controller
                    .connection_policy(&Self::device_identity(address), &connect_options(slot))
            })
    }

    fn refine(&mut self, address: DeviceAddr, sdp: SdpIdentity) -> polymer_bluetooth::Result<()> {
        let mut identity = Self::device_identity(address);
        identity.pnp = sdp.pnp;
        identity.service_uuids = sdp.services;
        if let Some(controller) = ControllerRegistry::standard().identify(&identity)? {
            self.profile_id = Some(controller.id());
        }
        Ok(())
    }

    pub(super) async fn establish(
        &mut self,
        host: &Arc<HciHost>,
        device: &InquiryResult,
        transport: &mut BrEdrHid,
    ) -> polymer_bluetooth::Result<()> {
        if self
            .token()
            .is_some_and(|id| id != "generic.hid" && id != "sony.hid")
        {
            return transport.connect_discovered(device).await;
        }
        // Sony models share names. Resolve their Device ID record on the same
        // ACL used for HID, before choosing a session or persisting its identity.
        let handle = host.connect(device).await?;
        let result = async {
            let discovered = sdp::query_identity(&Link::new(host.clone(), handle)).await?;
            self.refine(device.addr, discovered)?;
            diagnostic(&format!(
                "SDP identified {} as {}",
                device.addr,
                self.label()
            ));
            transport
                .establish_handle(handle, self.policy(device.addr, 0).controller_opens_hid)
                .await
        }
        .await;
        if result.is_err() {
            let _ = host.disconnect(handle).await;
        }
        result
    }
}
