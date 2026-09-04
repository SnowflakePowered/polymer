use polymer_bluetooth::transport::{
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

async fn run() -> polymer_bluetooth::Result<()> {
    let selector = parse_selector()?;
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

    match (selector.vendor_id, selector.product_id) {
        (Some(vendor), Some(product)) => {
            println!("Opening {vendor:04x}:{product:04x} exclusively...")
        }
        _ => println!("Opening the first WinUSB Bluetooth adapter exclusively..."),
    }
    let host = HciHost::open(&selector).await?;
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

fn parse_selector() -> polymer_bluetooth::Result<AdapterSelector> {
    let mut args = std::env::args().skip(1);
    let Some(vendor) = args.next() else {
        return Ok(AdapterSelector::first_winusb());
    };
    let product = args.next().ok_or(polymer_bluetooth::Error::InvalidPacket(
        "provide both VID and PID",
    ))?;
    let parse = |value: &str| {
        u16::from_str_radix(value.trim_start_matches("0x"), 16).map_err(|_| {
            polymer_bluetooth::Error::InvalidPacket("VID and PID must be hexadecimal u16 values")
        })
    };
    Ok(AdapterSelector::from_vid_pid(
        parse(&vendor)?,
        parse(&product)?,
    ))
}
