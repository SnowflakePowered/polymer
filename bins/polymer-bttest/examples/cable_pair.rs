//! Inspect USB controllers or pair one to the appropriate wireless adapter.
use polymer_bluetooth::{
    DeviceAddr, TransportKind,
    cable_pairing::{CablePairing, CablePairingHost},
    store::SqliteControllerStore,
    transport::bredr::hci::{AdapterSelector, HciHost},
};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = std::env::args()
        .nth(1)
        .ok_or("usage: cable_pair --list | <controller Bluetooth address>")?;
    let expected = if argument == "--list" {
        None
    } else {
        Some(argument.parse::<DeviceAddr>()?)
    };
    let mut selected = None;
    let devices = CablePairing::enumerate().await?;
    if devices.is_empty() {
        println!("No supported USB controllers found.");
    }
    for device in devices {
        let inspection = async {
            let mut session = device.open().await?;
            let state = session.read_pairing().await?;
            Ok::<_, polymer_bluetooth::Error>((session, state))
        }
        .await;
        let (session, state) = match inspection {
            Ok(result) => result,
            Err(error) => {
                eprintln!("Could not inspect {}: {error}", device.name);
                continue;
            }
        };
        if expected.is_none() {
            println!(
                "{}: {} via {}; saved host {:?}",
                state.controller_address,
                device.name,
                device.access_label(),
                state.host_address
            );
            continue;
        }
        if Some(state.controller_address) == expected {
            if selected.is_some() {
                return Err("ambiguous controller selection".into());
            }
            println!(
                "{} via {}; previous host {:?}",
                state.controller_address,
                device.access_label(),
                state.host_address
            );
            selected = Some(session);
        }
    }
    if expected.is_none() {
        return Ok(());
    }
    let mut session = selected.ok_or("requested USB controller not found")?;
    let directories = polymer_library::Directories::discover()?;
    let path = directories.storage().join("input.db");
    let coordinator = CablePairing::new(Arc::new(SqliteControllerStore::open(&path)?));
    let adapter;
    let address = if session.identity().wireless_transport() == TransportKind::Ble {
        polymer_bluetooth::transport::ble::default_adapter_address().await?
    } else {
        adapter = HciHost::open(&AdapterSelector::first_winusb()).await?;
        adapter.info().address
    };
    let host = CablePairingHost {
        address,
        adapter_index: (session.identity().wireless_transport() == TransportKind::Ble).then_some(0),
    };
    println!("Wireless host: {}", host.address);
    let verified = coordinator.pair(session.as_mut(), &host).await?;
    println!(
        "Verified {} paired to {}; saved to {}",
        verified.state.controller_address,
        host.address,
        path.display()
    );
    Ok(())
}
