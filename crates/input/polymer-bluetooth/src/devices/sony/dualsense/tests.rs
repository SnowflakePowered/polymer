use super::*;
use crate::{ConnectionState, DeviceAddr, ReportReceiver};
use bytes::Bytes;
use protocol::Report;
use tokio::sync::broadcast;

struct MockTransport {
    input: broadcast::Sender<Bytes>,
    writes: Vec<Vec<u8>>,
    reject: bool,
    valid_input: bool,
    factory: bool,
    disconnected: Option<broadcast::Sender<()>>,
}

impl Default for MockTransport {
    fn default() -> Self {
        Self {
            input: broadcast::channel(8).0,
            writes: Vec::new(),
            reject: false,
            valid_input: true,
            factory: true,
            disconnected: None,
        }
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn get_report(
        &mut self,
        kind: crate::transport::HidReportType,
        id: u8,
        size: u16,
    ) -> Result<Bytes> {
        assert_eq!(kind, crate::transport::HidReportType::Feature);
        assert_eq!((id, size), (5, 41));
        if !self.factory {
            return Err(Error::Unsupported("no factory calibration"));
        }
        Ok(Bytes::from_static(include_bytes!(
            "fixtures/calibration.bin"
        )))
    }
    async fn connect(&mut self, _: DeviceAddr) -> Result<()> {
        Ok(())
    }
    fn reports(&self) -> ReportReceiver {
        self.input.subscribe()
    }
    fn disconnections(&self) -> Option<crate::transport::DisconnectReceiver> {
        self.disconnected.as_ref().map(|sender| sender.subscribe())
    }
    async fn send(&mut self, data: &[u8]) -> Result<()> {
        if self.reject {
            return Err(Error::InvalidPacket("rejected output"));
        }
        self.writes.push(data.to_vec());
        if let Some(sender) = &self.disconnected {
            sender.send(()).unwrap();
            return Ok(());
        }
        if self.writes.len() == 1 {
            // Input arrives synchronously with activation, before send returns.
            let mut input = [0; 78];
            input[0] = 0x31;
            input[29..33].copy_from_slice(&10_200_000u32.to_le_bytes());
            let crc = Report::checksum(0xa1, &input[..74]);
            input[74..].copy_from_slice(&crc.to_le_bytes());
            if !self.valid_input {
                input[4] ^= 1;
            }
            self.input.send(Bytes::copy_from_slice(&input)).unwrap();
        }
        Ok(())
    }
    async fn disconnect(&mut self) -> Result<()> {
        Ok(())
    }
    fn state(&self) -> ConnectionState {
        ConnectionState::Connected
    }
}

#[tokio::test]
async fn disconnect_during_activation_fails_without_waiting_for_input_timeout() {
    let mut transport = MockTransport {
        disconnected: Some(broadcast::channel(1).0),
        ..Default::default()
    };
    let mut session = DualSense.connect(&ConnectOptions::default());
    let result = tokio::time::timeout(Duration::from_secs(1), session.synchronize(&mut transport))
        .await
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidPacket(_))));
    assert_eq!(transport.writes.len(), 1);
}

#[tokio::test]
async fn early_input_completes_activation_and_feedback_preserves_other_fields() {
    let mut transport = MockTransport::default();
    let mut session = DualSense.connect(&ConnectOptions::default());
    session.synchronize(&mut transport).await.unwrap();
    assert_eq!(
        session.sensor_calibration().unwrap().gyroscope_bias,
        [100.0, -50.0, 25.0]
    );
    assert_eq!(transport.writes.len(), 3);
    assert_eq!(transport.writes[1][41], 2);
    session
        .set_rumble(&mut transport, 0xffff, 0x8000)
        .await
        .unwrap();
    session.set_player_led(&mut transport, 2).await.unwrap();
    let last = transport.writes.last().unwrap();
    assert_eq!(&last[5..7], &[64, 127]);
    assert_eq!(last[46], 21);
    for (sequence, report) in transport.writes.iter().enumerate() {
        assert_eq!(report[1], (sequence as u8) << 4);
        assert!(Report::valid(report, 0x31, 78, 0xa2));
    }
}

#[tokio::test]
async fn unavailable_factory_calibration_does_not_prevent_input() {
    let mut transport = MockTransport {
        factory: false,
        ..Default::default()
    };
    let mut session = DualSense.connect(&ConnectOptions::default());
    session.synchronize(&mut transport).await.unwrap();
    assert!(session.sensor_calibration().is_none());
    assert_eq!(transport.writes.len(), 3);
}

#[tokio::test]
async fn corrupt_input_never_claims_readiness() {
    let mut transport = MockTransport {
        valid_input: false,
        ..Default::default()
    };
    let mut session = DualSense.connect(&ConnectOptions::default());
    assert!(matches!(
        session.synchronize(&mut transport).await,
        Err(Error::Timeout(_))
    ));
    assert_eq!(transport.writes.len(), 1);
}

#[tokio::test]
async fn failed_output_does_not_commit_state_or_sequence() {
    let mut transport = MockTransport::default();
    let mut session = DualSense.connect(&ConnectOptions::default());
    session.synchronize(&mut transport).await.unwrap();
    transport.reject = true;
    assert!(
        session
            .set_rumble(&mut transport, 0xffff, 0xffff)
            .await
            .is_err()
    );
    transport.reject = false;
    for _ in 0..16 {
        session.set_player_led(&mut transport, 1).await.unwrap();
    }
    assert_eq!(&transport.writes[3][5..7], &[0, 0]);
    assert_eq!(transport.writes[3][1], 0x30);
    assert_eq!(transport.writes[16][1], 0);
}
