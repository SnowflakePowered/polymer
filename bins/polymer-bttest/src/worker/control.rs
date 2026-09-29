use super::*;

pub(super) async fn stop_br_scan(
    task: &mut Option<JoinHandle<()>>,
    host: Option<&Arc<HciHost>>,
    events: &mpsc::Sender<Event>,
) {
    let Some(task) = task.take() else {
        return;
    };
    task.abort();
    let _ = task.await;
    if let Some(host) = host {
        let _ = host.cancel_inquiry().await;
    }
    diagnostic("Continuous BR/EDR inquiry disabled");
    let _ = events.send(Event::Scanning {
        radio: RadioKind::BrEdr,
        enabled: false,
        message: "BR/EDR scanning stopped".into(),
    });
}

pub(super) async fn stop_ble_scan(
    task: &mut Option<(usize, JoinHandle<()>)>,
    events: &mpsc::Sender<Event>,
) {
    let Some((adapter_index, task)) = task.take() else {
        return;
    };
    task.abort();
    let _ = task.await;
    if let Err(error) = stop_ble_adapter_scan(adapter_index).await {
        diagnostic(&format!("Could not stop OS BLE scan cleanly: {error}"));
    }
    diagnostic("Continuous BLE scan disabled");
    let _ = events.send(Event::Scanning {
        radio: RadioKind::Ble,
        enabled: false,
        message: "BLE scanning stopped".into(),
    });
}

pub(super) async fn stop_ble_recovery(task: &mut Option<(usize, JoinHandle<()>)>) {
    let Some((adapter_index, task)) = task.take() else {
        return;
    };
    task.abort();
    let _ = task.await;
    let _ = stop_ble_adapter_scan(adapter_index).await;
}

pub(super) fn configure_pin(
    pairing: &PairingAgent,
    addr: DeviceAddr,
    choice: BondingMode,
    identity: &BrIdentity,
) -> polymer_bluetooth::Result<()> {
    match choice {
        BondingMode::Ssp if identity.is_switch() => {
            pairing.set_pin(addr, PinCode::Fixed(b"0000".to_vec()))
        }
        BondingMode::Ssp => pairing.clear_pin(addr),
        BondingMode::WiiOneTwo => pairing.set_pin(addr, PinCode::RemoteAddress),
        BondingMode::WiiSync => pairing.set_pin(addr, PinCode::LocalAddress),
    }
}

pub(super) fn next_slot(
    active: &HashMap<DeviceAddr, ActiveController>,
    pending: &HashMap<DeviceAddr, PendingInbound>,
    outbound: &HashMap<DeviceAddr, PendingOutbound>,
    ble: &HashMap<DeviceAddr, PendingBle>,
) -> usize {
    (0..4)
        .find(|slot| {
            !active.values().any(|controller| controller.slot == *slot)
                && !pending.values().any(|controller| controller.slot == *slot)
                && !outbound.values().any(|controller| controller.slot == *slot)
                && !ble.values().any(|controller| controller.slot == *slot)
        })
        .unwrap_or_else(|| (active.len() + pending.len() + outbound.len() + ble.len()) % 4)
}

pub(super) fn start_controller_order_scans(
    adapter_index: usize,
    scan_radios: (bool, bool),
    host: Option<&Arc<HciHost>>,
    events: &mpsc::Sender<Event>,
    ble_scan_gate: &Arc<Mutex<()>>,
    br_scan_task: &mut Option<JoinHandle<()>>,
    ble_scan_task: &mut Option<(usize, JoinHandle<()>)>,
) {
    let (scan_br, scan_ble) = scan_radios;
    if scan_br
        && br_scan_task.is_none()
        && let Some(host) = host
    {
        diagnostic("Controller order: BR/EDR inquiry enabled");
        *br_scan_task = Some(spawn_br_scan(host.clone(), events.clone()));
        let _ = events.send(Event::Scanning {
            radio: RadioKind::BrEdr,
            enabled: true,
            message: "Controller order: BR/EDR discovery active".into(),
        });
    }
    if scan_ble && ble_scan_task.is_none() {
        diagnostic(&format!(
            "Controller order: BLE discovery enabled on OS adapter {adapter_index}"
        ));
        *ble_scan_task = Some((
            adapter_index,
            spawn_ble_scan(adapter_index, events.clone(), ble_scan_gate.clone()),
        ));
        let _ = events.send(Event::Scanning {
            radio: RadioKind::Ble,
            enabled: true,
            message: "Controller order: Switch 2 BLE discovery active".into(),
        });
    }
}

pub(super) async fn sleep_all_for_controller_order(
    active: &mut HashMap<DeviceAddr, ActiveController>,
    events: &mpsc::Sender<Event>,
) {
    let addresses = active.keys().copied().collect::<Vec<_>>();
    for addr in addresses {
        let Some(mut controller) = active.remove(&addr) else {
            continue;
        };
        match &mut controller.transport {
            ActiveTransport::Ble(_) => {
                if let Err(error) = controller.sleep().await {
                    let _ = events.send(Event::Log(format!(
                        "Switch 2 sleep request for {addr} failed; disconnecting instead: {error}"
                    )));
                } else {
                    // The Nintendo sleep characteristic is write-without-response.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            ActiveTransport::BrEdr(_) if controller.identity.is_switch() => {
                if let Err(error) = controller.sleep().await {
                    let _ = events.send(Event::Log(format!(
                        "Nintendo sleep request for {addr} failed; disconnecting instead: {error}"
                    )));
                }
            }
            ActiveTransport::BrEdr(_) => {}
        }
        controller.report_task.abort();
        let _ = controller.transport.disconnect().await;
        let _ = events.send(Event::Disconnected {
            addr: addr.to_string(),
            reason: "Waiting to rejoin in controller order".into(),
        });
    }
}

pub(super) async fn stop_active(
    active: &mut HashMap<DeviceAddr, ActiveController>,
    addr: DeviceAddr,
) {
    if let Some(mut controller) = active.remove(&addr) {
        controller.report_task.abort();
        let _ = controller.transport.disconnect().await;
    }
}

pub(super) async fn disconnect_active(
    active: &mut HashMap<DeviceAddr, ActiveController>,
    addr: DeviceAddr,
    events: &mpsc::Sender<Event>,
) {
    let Some(mut controller) = active.remove(&addr) else {
        return;
    };
    if controller.identity.is_switch() && matches!(&controller.transport, ActiveTransport::BrEdr(_))
    {
        match controller.sleep().await {
            Ok(()) => {
                let _ = events.send(Event::Log(format!(
                    "Requested Nintendo controller {addr} sleep before disconnect"
                )));
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!(
                    "Nintendo sleep request for {addr} failed: {error}"
                )));
            }
        }
    }
    controller.report_task.abort();
    let _ = controller.transport.disconnect().await;
    let _ = events.send(Event::Disconnected {
        addr: addr.to_string(),
        reason: "Disconnected".into(),
    });
}

pub(super) async fn stop_all_active(active: &mut HashMap<DeviceAddr, ActiveController>) {
    let addresses = active.keys().copied().collect::<Vec<_>>();
    for addr in addresses {
        stop_active(active, addr).await;
    }
}
