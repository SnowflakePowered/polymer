use super::*;
use crate::{ConnectionState, DeviceAddr, ReportReceiver};
use bytes::Bytes;
use tokio::sync::broadcast;

struct MockTransport {
    input: broadcast::Sender<Bytes>,
    writes: Vec<(HidReportType, Vec<u8>)>,
    reject_feature: bool,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            input: broadcast::channel(8).0,
            writes: Vec::new(),
            reject_feature: false,
        }
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn connect(&mut self, _: DeviceAddr) -> Result<()> {
        Ok(())
    }
    fn reports(&self) -> ReportReceiver {
        self.input.subscribe()
    }
    async fn send(&mut self, _: &[u8]) -> Result<()> {
        panic!("DS3 activation must use the control channel")
    }
    async fn set_report(&mut self, kind: HidReportType, data: &[u8]) -> Result<()> {
        self.writes.push((kind, data.to_vec()));
        if kind == HidReportType::Feature && self.reject_feature {
            return Err(Error::InvalidPacket("feature rejected"));
        }
        if kind == HidReportType::Output {
            let mut report = vec![0; 49];
            report[0] = 1;
            let _ = self.input.send(Bytes::from(report));
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
async fn activation_enables_feature_before_output_and_observes_early_input() {
    let mut transport = MockTransport::new();
    let mut session = DualShock3.connect(&ConnectOptions {
        player_index: 2,
        auto_reconnect: true,
    });
    session.synchronize(&mut transport).await.unwrap();
    assert_eq!(
        transport.writes[0],
        (HidReportType::Feature, vec![0xf4, 0x42, 3, 0, 0])
    );
    assert_eq!(transport.writes[1].0, HidReportType::Output);
    assert_eq!(transport.writes[1].1[10], 0x08);
    assert_eq!(transport.writes[1].1[3], 0);
    assert_eq!(transport.writes[1].1[5], 0);
    session.set_rumble(&mut transport, 0x8000, 1).await.unwrap();
    assert_eq!(transport.writes[2].1[3], 1);
    assert_eq!(transport.writes[2].1[5], 0x80);
    assert_eq!(transport.writes[2].1[10], 0x08);
}

#[tokio::test]
async fn rejected_activation_does_not_send_output_or_claim_readiness() {
    let mut transport = MockTransport::new();
    transport.reject_feature = true;
    let mut session = DualShock3.connect(&ConnectOptions::default());
    assert!(session.synchronize(&mut transport).await.is_err());
    assert_eq!(transport.writes.len(), 1);
}
