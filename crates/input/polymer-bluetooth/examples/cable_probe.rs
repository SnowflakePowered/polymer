//! Read-only cable-pairing diagnostics. Never writes the controller's host address.

use polymer_bluetooth::cable_pairing::CablePairing;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for device in CablePairing::enumerate().await? {
        println!(
            "{} · {} · {}",
            device.name,
            device.access_label(),
            device.path().to_string_lossy()
        );
        let result = async {
            let mut session = device.open().await?;
            session.read_pairing().await
        }
        .await;
        match result {
            Ok(state) => println!(
                "Controller {} · saved host {:?}",
                state.controller_address, state.host_address
            ),
            Err(error) => println!("{error}"),
        }
    }
    Ok(())
}
