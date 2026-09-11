//! Bounded live check of a persisted DS3's inbound HID setup and input activation.
use polymer_bluetooth::{
    DeviceAddr, Transport, TransportKind,
    devices::{ConnectOptions, ControllerRegistry, ProfileId},
    store::{ControllerStore, SqliteControllerStore},
    transport::bredr::{
        BrEdrHid,
        hci::{AdapterSelector, Event, HciHost},
    },
};
use std::{sync::Arc, time::Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("polymer_bluetooth=debug")
        .with_target(true)
        .init();
    let addr: DeviceAddr = std::env::args()
        .nth(1)
        .ok_or("usage: ds3_bluetooth <controller Bluetooth address>")?
        .parse()?;
    let dirs = polymer_library::Directories::discover()?;
    let store = SqliteControllerStore::open(dirs.storage().join("input.db"))?;
    let record = store
        .records()?
        .into_iter()
        .find(|record| {
            record.address == addr
                && record.transport == TransportKind::BrEdr
                && record.profile_id.as_str() == "sony.dualshock3"
        })
        .ok_or("controller has no persisted DS3 cable pairing")?;
    let host = Arc::new(HciHost::open(&AdapterSelector::first_winusb()).await?);
    let mut events = host.events();
    let frames = host.l2cap_frames();
    println!(
        "READY: adapter {}; unplug USB and press PS on {addr}",
        host.info().address
    );
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if let Event::ConnectionRequest { addr: incoming, .. } = events
                .recv()
                .await
                .map_err(|_| polymer_bluetooth::Error::ChannelClosed)?
            {
                if incoming == addr {
                    break;
                }
            }
        }
        Ok::<_, polymer_bluetooth::Error>(())
    })
    .await??;
    let handle = host.accept_connection_with_role(addr, true).await?;
    let mut transport = BrEdrHid::new(host);
    transport.set_automatic_flush_slots(0);
    let result = async {
        transport.attach_inbound_with_frames(handle, frames).await?;
        let profile = ControllerRegistry::standard()
            .get(&ProfileId::new("sony.dualshock3"))
            .unwrap();
        let mut session = profile.connect(&ConnectOptions {
            player_index: record.player_index,
            auto_reconnect: true,
        });
        let mut reports = transport.reports();
        session.synchronize(&mut transport).await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            for index in 0..100 {
                let report = reports
                    .recv()
                    .await
                    .map_err(|_| polymer_bluetooth::Error::ChannelClosed)?;
                if index < 3 {
                    println!("INPUT {} bytes: {:02x?}", report.len(), report);
                }
            }
            Ok::<_, polymer_bluetooth::Error>(())
        })
        .await
        .map_err(|_| polymer_bluetooth::Error::Timeout("DS3 live input sample"))??;
        println!("PASS: DS3 activated and delivered 100 Bluetooth input reports");
        Ok::<_, polymer_bluetooth::Error>(())
    }
    .await;
    let _ = transport.disconnect().await;
    result?;
    Ok(())
}
