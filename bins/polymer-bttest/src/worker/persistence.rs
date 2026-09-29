use super::*;

pub(super) fn load_known_ble_controllers(
    store: &dyn ControllerStore,
) -> HashMap<DeviceAddr, KnownBle> {
    let Ok(records) = store.records() else {
        return HashMap::new();
    };
    records
        .into_iter()
        .filter_map(|record| {
            if record.transport != TransportKind::Ble
                || !record.profile_id.as_str().starts_with("nintendo.switch2.")
            {
                return None;
            }
            let controller = ControllerRegistry::standard().get(&record.profile_id)?;
            Some((
                record.address,
                KnownBle {
                    adapter_index: record.adapter_index.unwrap_or(0),
                    device: BleDevice {
                        addr: record.address,
                        name: Some(controller.display_name().into()),
                        rssi: None,
                        services: Vec::new(),
                        manufacturer_data: HashMap::new(),
                    },
                    profile_id: record.profile_id,
                    slot: usize::from(record.player_index.min(3)),
                },
            ))
        })
        .collect()
}

pub(super) fn save_known_ble_controllers(
    store: &dyn ControllerStore,
    controllers: &HashMap<DeviceAddr, KnownBle>,
) -> polymer_bluetooth::Result<()> {
    for record in store.records()? {
        if record.transport == TransportKind::Ble
            && record.profile_id.as_str().starts_with("nintendo.switch2.")
            && !controllers.contains_key(&record.address)
        {
            store.remove(record.address)?;
        }
    }
    for known in controllers.values() {
        store.put(ControllerRecord {
            address: known.device.addr,
            transport: TransportKind::Ble,
            profile_id: known.profile_id.clone(),
            link_key: None,
            link_key_type: None,
            adapter_index: Some(known.adapter_index),
            auto_reconnect: true,
            player_index: u8::try_from(known.slot).unwrap_or(u8::MAX),
        })?;
    }
    Ok(())
}

pub(super) fn load_controller_identities(
    store: &dyn ControllerStore,
) -> HashMap<DeviceAddr, BrIdentity> {
    store
        .records()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|record| {
            (record.transport == TransportKind::BrEdr)
                .then(|| {
                    BrIdentity::from_profile_id(record.profile_id.as_str())
                        .map(|identity| (record.address, identity))
                })
                .flatten()
        })
        .collect()
}

pub(super) fn save_controller_identities(
    store: &dyn ControllerStore,
    identities: &HashMap<DeviceAddr, BrIdentity>,
) -> polymer_bluetooth::Result<()> {
    for (addr, identity) in identities {
        let Some(profile_id) = identity.token() else {
            continue;
        };
        let previous = store
            .records()?
            .into_iter()
            .find(|record| record.address == *addr);
        store.put(ControllerRecord {
            address: *addr,
            transport: TransportKind::BrEdr,
            profile_id: polymer_bluetooth::devices::ProfileId::new(profile_id),
            link_key: previous.as_ref().and_then(|record| record.link_key),
            link_key_type: previous.as_ref().and_then(|record| record.link_key_type),
            adapter_index: None,
            auto_reconnect: true,
            player_index: previous.as_ref().map_or(0, |record| record.player_index),
        })?;
    }
    Ok(())
}

fn diagnostic_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Polymer").join("bluetooth-test.log")
}

pub(super) fn reset_diagnostic_log() {
    let path = diagnostic_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, "Polymer Bluetooth test diagnostics\n");
}

pub(super) fn diagnostic(message: &str) {
    eprintln!("{message}");
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(diagnostic_path())
    {
        let _ = writeln!(file, "{message}");
    }
}
