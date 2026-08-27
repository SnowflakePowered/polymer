use snowglass_bluetooth::{
    ble::BleGatt,
    bredr::hci::{AdapterSelector, HciHost, UsbHci},
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Bluetooth HCI probe failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> snowglass_bluetooth::Result<()> {
    let (vendor_id, product_id) = parse_vid_pid()?;
    println!("OS-owned BLE adapters:");
    for adapter in BleGatt::adapter_names().await? {
        println!("  {adapter}");
    }
    let adapters = UsbHci::adapters().await?;
    println!("Discovered USB Bluetooth adapters:");
    for adapter in &adapters {
        println!(
            "  {:04x}:{:04x} {}",
            adapter.vendor_id,
            adapter.product_id,
            adapter.product.as_deref().unwrap_or("(unnamed)")
        );
    }
    if adapters.is_empty() {
        println!("  (none)");
    }

    println!("Opening {vendor_id:04x}:{product_id:04x} exclusively...");
    let host = HciHost::open(&AdapterSelector::from_vid_pid(vendor_id, product_id)).await?;
    let info = host.info();
    println!("Adapter address: {}", info.address);
    println!(
        "Version: HCI {} rev {:04x}, LMP {} subver {:04x}, manufacturer {}",
        info.hci_version,
        info.hci_revision,
        info.lmp_version,
        info.lmp_subversion,
        info.manufacturer
    );
    if let Some(source) = &info.firmware_source {
        println!("Firmware initialized from: {source}");
    }
    println!("Local features: {:02x?}", info.features);
    println!(
        "BR/EDR ACL: {} byte packets, {} controller buffers",
        info.acl_data_packet_length, info.total_acl_packets
    );
    match (info.le_acl_data_packet_length, info.total_le_acl_packets) {
        (Some(length), Some(count)) => {
            println!("LE ACL: {length} byte packets, {count} controller buffers")
        }
        _ => println!("LE ACL: not reported"),
    }
    Ok(())
}

fn parse_vid_pid() -> snowglass_bluetooth::Result<(u16, u16)> {
    let mut args = std::env::args().skip(1);
    let vendor = args.next().unwrap_or_else(|| "2357".into());
    let product = args.next().unwrap_or_else(|| "0604".into());
    let parse = |value: &str| {
        u16::from_str_radix(value.trim_start_matches("0x"), 16).map_err(|_| {
            snowglass_bluetooth::Error::InvalidPacket("VID and PID must be hexadecimal u16 values")
        })
    };
    Ok((parse(&vendor)?, parse(&product)?))
}
