//! Attempts the same unelevated driver switch as the debug UI, for an explicit HID path.

#[path = "../src/cable_pairing/driver/mod.rs"]
mod driver;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: driver_switch <DS3 HID device path>")?;
    let device = polymer_bluetooth::cable_pairing::CablePairing::enumerate()
        .await?
        .into_iter()
        .find(|device| device.path().to_string_lossy() == path)
        .ok_or("requested DS3 is not connected")?;
    println!("{}", driver::DriverSwitch::switch(device).await?);
    Ok(())
}
