//! Cable-provisioned, controller-initiated Bluetooth HID lifecycle.
use crate::{
    Error, Result, Transport, TransportKind,
    devices::{
        ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerSession, DeviceIdentity,
        MatchConfidence, PairingPolicy, ProfileId, ReconnectPolicy, VendorProductId,
    },
    transport::HidReportType,
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::JoinHandle};
pub mod motion;
mod runtime;
use motion::{MotionCalibration, MotionPath, MotionState};

#[derive(Default)]
pub struct DualShock3;

#[async_trait]
impl ControllerDevice for DualShock3 {
    fn id(&self) -> ProfileId {
        ProfileId::new("sony.dualshock3")
    }
    fn display_name(&self) -> &'static str {
        "DualShock 3"
    }
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        if identity.transport != TransportKind::BrEdr {
            return None;
        }
        if identity.has_pnp(0x054c, &[0x0268, 0x042f]) {
            return Some(MatchConfidence::Pnp);
        }
        identity
            .name_is("PLAYSTATION(R)3 Controller")
            .then_some(MatchConfidence::Model)
    }
    fn vendor_product(&self, identity: &DeviceIdentity) -> Option<VendorProductId> {
        identity
            .vendor_product()
            .or(Some(VendorProductId::new(0x054c, 0x0268)))
    }
    fn serial(&self, identity: &DeviceIdentity) -> Option<Arc<str>> {
        Some(Arc::from(identity.address.to_string()))
    }
    fn connection_policy(&self, _: &DeviceIdentity, _: &ConnectOptions) -> ConnectionPolicy {
        ConnectionPolicy {
            pairing: PairingPolicy::PseudoOob,
            reconnect: ReconnectPolicy::ControllerInitiated,
            controller_opens_hid: true,
            wait_for_encryption_before_inbound_hid: false,
            automatic_flush_slots: Some(0),
            ..ConnectionPolicy::default()
        }
    }
    fn preferred_input_report_id(&self) -> Option<u8> {
        Some(1)
    }
    fn connect(&self, options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(Session {
            output: Arc::new(Mutex::new(Output {
                player: options.player_index,
                low: 0,
                high: 0,
                motion: None,
            })),
            motion: MotionState::default(),
            tracker: None,
        })
    }
}

struct Session {
    output: Arc<Mutex<Output>>,
    motion: MotionState,
    tracker: Option<(JoinHandle<()>, tokio::sync::oneshot::Sender<()>)>,
}

#[derive(Clone)]
struct Output {
    player: u8,
    low: u16,
    high: u16,
    motion: Option<MotionCalibration>,
}

impl Output {
    fn report(&self) -> Result<[u8; 49]> {
        let leds = [0x02, 0x04, 0x08, 0x10, 0x12, 0x14, 0x18];
        let led = *leds
            .get(usize::from(self.player))
            .ok_or(Error::InvalidPacket("DS3 player index must be 0..6"))?;
        let mut report = [0; 49];
        report[0] = 1;
        report[2] = 0xff;
        report[3] = u8::from(self.high != 0);
        report[4] = 0xff;
        report[5] = (self.low >> 8) as u8;
        report[10] = led;
        for block in report[11..31].chunks_exact_mut(5) {
            block.copy_from_slice(&[0xff, 0x27, 0x10, 0, 0x32]);
        }
        if let Some(motion) = self.motion {
            motion.apply_output(&mut report);
        }
        Ok(report)
    }
}

impl Session {
    async fn stop_tracker(&mut self) {
        if let Some((task, stop)) = self.tracker.take() {
            let _ = stop.send(());
            let _ = task.await;
        }
    }

    fn start_tracker(&mut self, transport: &dyn Transport, calibration: MotionCalibration) {
        if calibration.path == MotionPath::Plain {
            return;
        }
        let Some(control) = transport.hid_control() else {
            return;
        };
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let runtime = runtime::Runtime::new(
            self.output.clone(),
            self.motion.clone(),
            control,
            calibration,
        );
        let reports = transport.reports();
        let task = tokio::spawn(runtime.run(reports, stopped));
        self.tracker = Some((task, stop));
    }
}

#[async_trait]
impl ControllerSession for Session {
    fn ds3_motion(&self) -> Option<MotionState> {
        Some(self.motion.clone())
    }

    async fn synchronize(&mut self, transport: &mut dyn Transport) -> Result<()> {
        self.stop_tracker().await;
        self.motion.clear();
        let mut input = transport.reports();
        transport
            .set_report(HidReportType::Feature, &[0xf4, 0x42, 0x03, 0, 0])
            .await?;
        // Unsupported feature reports leave the nominal decoder available. Never
        // guess a trim byte: some DS3s use this slot differently.
        let calibration = match MotionCalibration::read(transport).await {
            Ok(calibration) => {
                tracing::info!(?calibration, "DS3 factory motion calibration loaded");
                Some(calibration)
            }
            Err(error) => {
                tracing::warn!(%error, "DS3 factory motion calibration unavailable");
                None
            }
        };
        {
            let mut output = self.output.lock().await;
            output.motion = calibration;
            transport
                .set_report(HidReportType::Output, &output.report()?)
                .await?;
        }
        if let Some(calibration) = calibration {
            self.motion.publish(calibration);
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match input.recv().await {
                    Ok(report) if report.len() == 49 && report[0] == 1 => return Ok(()),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Err(Error::ChannelClosed),
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("DS3 first Bluetooth input report"))??;
        if let Some(calibration) = calibration {
            self.start_tracker(transport, calibration);
        }
        tracing::info!("DS3 Bluetooth input activated");
        Ok(())
    }

    async fn set_external_ownership(
        &mut self,
        transport: &mut dyn Transport,
        enabled: bool,
    ) -> Result<()> {
        self.stop_tracker().await;
        // Wait for any in-flight output transaction to release its state lock.
        {
            let _guard = self.output.lock().await;
        }
        if enabled {
            Ok(())
        } else {
            self.synchronize(transport).await
        }
    }

    async fn set_player_led(&mut self, transport: &mut dyn Transport, slot: u8) -> Result<()> {
        let mut output = self.output.lock().await;
        let mut next = output.clone();
        next.player = slot;
        transport
            .set_report(HidReportType::Output, &next.report()?)
            .await?;
        *output = next;
        Ok(())
    }

    async fn set_rumble(
        &mut self,
        transport: &mut dyn Transport,
        low: u16,
        high: u16,
    ) -> Result<()> {
        let mut output = self.output.lock().await;
        let mut next = output.clone();
        next.low = low;
        next.high = high;
        transport
            .set_report(HidReportType::Output, &next.report()?)
            .await?;
        *output = next;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some((task, _)) = self.tracker.take() {
            task.abort();
        }
    }
}
