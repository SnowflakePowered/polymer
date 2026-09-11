//! Explicitly pair one USB controller to the dedicated adapter and persist the bond.
use polymer_bluetooth::{
    DeviceAddr,
    cable_pairing::{CablePairing, CablePairingHost},
    store::SqliteControllerStore,
    transport::bredr::hci::{AdapterSelector, HciHost},
};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let expected: DeviceAddr = std::env::args()
        .nth(1)
        .ok_or("usage: cable_pair <controller Bluetooth address>")?
        .parse()?;
    let mut selected = None;
    for device in CablePairing::enumerate().await? {
        let mut session = device.open().await?;
        let state = session.read_pairing().await?;
        if state.controller_address == expected {
            if selected.is_some() {
                return Err("ambiguous controller selection".into());
            }
            println!(
                "{} via {}; previous host {:?}",
                expected,
                device.access_label(),
                state.host_address
            );
            selected = Some(session);
        }
    }
    let mut session = selected.ok_or("requested USB controller not found")?;
    let directories = polymer_library::Directories::discover()?;
    let path = directories.storage().join("input.db");
    let coordinator = CablePairing::new(Arc::new(SqliteControllerStore::open(&path)?));
    let adapter = HciHost::open(&AdapterSelector::first_winusb()).await?;
    let host = CablePairingHost {
        address: adapter.info().address,
    };
    println!("Dedicated adapter: {}", host.address);
    let verified = coordinator.pair(session.as_mut(), &host).await?;
    println!(
        "Verified {} paired to {}; saved to {}",
        verified.state.controller_address,
        host.address,
        path.display()
    );
    Ok(())
}
