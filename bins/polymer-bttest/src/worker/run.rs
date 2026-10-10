use super::*;

pub(super) async fn run(
    mut commands: tokio_mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let (controller_store, cable_registry_error): (Arc<dyn ControllerStore>, Option<String>) =
        match polymer_library::Directories::discover()
            .map_err(|error| error.to_string())
            .and_then(|directories| {
                SqliteControllerStore::open(directories.storage().join("input.db"))
                    .map_err(|error| error.to_string())
            }) {
            Ok(store) => (Arc::new(store), None),
            Err(error) => {
                let _ = events.send(Event::Log(format!(
                    "controller registry unavailable; using memory only: {error}"
                )));
                let _ = events.send(Event::CablePairing(
                    crate::cable_pairing::Event::RegistryUnavailable(error.clone()),
                ));
                (Arc::new(MemoryControllerStore::default()), Some(error))
            }
        };
    let ble_adapter_count = match BleGatt::adapter_names().await {
        Ok(names) if !names.is_empty() => {
            let count = names.len();
            diagnostic(&format!("OS BLE adapters: {}", names.join(", ")));
            let _ = events.send(Event::BleReady(names));
            count
        }
        Ok(_) => {
            let _ = events.send(Event::Log("No OS-owned BLE adapter found".into()));
            0
        }
        Err(error) => {
            diagnostic(&format!("BLE adapter error: {error}"));
            let _ = events.send(Event::Log(format!("BLE adapter error: {error}")));
            0
        }
    };

    let selector = AdapterSelector::first_winusb();
    let host = match HciHost::open(&selector).await {
        Ok(host) => Some(Arc::new(host)),
        Err(error) => {
            diagnostic(&format!("Dedicated BR/EDR adapter unavailable: {error}"));
            let _ = events.send(Event::Log(format!(
                "Dedicated BR/EDR adapter unavailable: {error}"
            )));
            None
        }
    };
    let link_keys: Arc<dyn LinkKeyStore> =
        Arc::new(ControllerLinkKeyStore::new(controller_store.clone()));
    let pairing = host
        .as_ref()
        .map(|host| Arc::new(PairingAgent::new(host.clone(), link_keys.clone())));
    let pairing_task = pairing.as_ref().map(|pairing| {
        let pairing = pairing.clone();
        let pairing_events = events.clone();
        tokio::spawn(async move {
            if let Err(error) = pairing.run().await {
                let _ = pairing_events.send(Event::Error(format!(
                    "BR/EDR pairing agent stopped: {error}"
                )));
            }
        })
    });
    if let Some(host) = &host {
        let info = host.info();
        let _ = events.send(Event::CablePairing(crate::cable_pairing::Event::Host(
            polymer_bluetooth::cable_pairing::CablePairingHost {
                address: info.address,
                adapter_index: None,
            },
        )));
        diagnostic(&format!(
            "BR/EDR adapter ready: {} (HCI {} rev {:04X}, LMP {} subver {:04X}, manufacturer {}; ACL {} bytes / {} buffers)",
            info.address,
            info.hci_version,
            info.hci_revision,
            info.lmp_version,
            info.lmp_subversion,
            info.manufacturer,
            info.acl_data_packet_length,
            info.total_acl_packets
        ));
        if let Some(source) = &info.firmware_source {
            diagnostic(&format!("Loaded adapter firmware from {source}"));
            let _ = events.send(Event::Log(format!(
                "Initialized dedicated radio firmware from {source}"
            )));
        }
        let _ = events.send(Event::BrReady(format!(
            "WinUSB {:04X}:{:04X} {} · ACL {} bytes / {} buffers",
            info.usb_vendor_id,
            info.usb_product_id,
            info.address,
            info.acl_data_packet_length,
            info.total_acl_packets
        )));
    }

    let mut hci_events = host.as_ref().map(|host| host.events());
    let mut active: HashMap<DeviceAddr, ActiveController> = HashMap::new();
    let mut pending_inbound: HashMap<DeviceAddr, PendingInbound> = HashMap::new();
    let (inbound_tx, mut inbound_rx) = tokio_mpsc::unbounded_channel::<InboundResult>();
    let mut pending_outbound: HashMap<DeviceAddr, PendingOutbound> = HashMap::new();
    let (outbound_tx, mut outbound_rx) = tokio_mpsc::unbounded_channel::<OutboundResult>();
    let mut pending_ble: HashMap<DeviceAddr, PendingBle> = HashMap::new();
    let (ble_tx, mut ble_rx) = tokio_mpsc::unbounded_channel::<BleResult>();
    let (ble_disconnect_tx, mut ble_disconnect_rx) =
        tokio_mpsc::unbounded_channel::<BleDisconnected>();
    let (ble_recovery_tx, mut ble_recovery_rx) = tokio_mpsc::unbounded_channel::<BleRecovery>();
    // WinRT exposes scanning as adapter-wide state. Every discovery path must
    // share this gate or one controller's stop_scan cancels another's wake.
    let ble_scan_gate = Arc::new(Mutex::new(()));
    let mut known_ble = load_known_ble_controllers(controller_store.as_ref());
    let mut restored_ble = known_ble.values().cloned().collect::<Vec<_>>();
    restored_ble.sort_by_key(|known| known.slot);
    for known in restored_ble {
        let addr = known.device.addr;
        diagnostic(&format!(
            "Restored bonded Switch 2 controller {addr} ({}) in player slot {}; waiting for wake advertisement",
            known.label(),
            known.slot + 1
        ));
        let _ = events.send(Event::Log(format!(
            "Bonded {} restored in player slot {}; press a controller button to reconnect",
            known.label(),
            known.slot + 1
        )));
        let pending = spawn_ble_connection(
            known.adapter_index,
            known.device,
            known.slot,
            true,
            events.clone(),
            ble_tx.clone(),
            ble_scan_gate.clone(),
        );
        pending_ble.insert(addr, pending);
    }
    let mut ble_recovery_task: Option<(usize, JoinHandle<()>)> = None;
    if known_ble.is_empty() && ble_adapter_count > 0 {
        match default_adapter_address().await {
            Ok(host_address) => {
                diagnostic(&format!(
                    "No persisted Switch 2 controllers; recovering wake advertisements bonded to {host_address}"
                ));
                let _ = events.send(Event::Log(format!(
                    "Looking for existing Switch 2 bonds to {host_address}; press HOME"
                )));
                ble_recovery_task = Some((
                    0,
                    spawn_ble_bond_recovery(
                        0,
                        host_address,
                        events.clone(),
                        ble_recovery_tx.clone(),
                        ble_scan_gate.clone(),
                    ),
                ));
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!(
                    "Could not recover existing Switch 2 bonds: {error}"
                )));
            }
        }
    }
    let mut br_scan_task: Option<JoinHandle<()>> = None;
    let mut ble_scan_task: Option<(usize, JoinHandle<()>)> = None;
    let mut controller_order_adapter = None;
    let mut identities = load_controller_identities(controller_store.as_ref());
    let cable_ble_host = default_adapter_address().await.ok().map(|address| {
        polymer_bluetooth::cable_pairing::CablePairingHost {
            address,
            adapter_index: Some(0),
        }
    });
    if let Some(host) = cable_ble_host {
        let _ = events.send(Event::CablePairing(crate::cable_pairing::Event::BleHost(
            host,
        )));
    }
    let mut cable_pairing = crate::cable_pairing::worker::CablePairingWorker::new(
        host.as_ref()
            .map(|host| polymer_bluetooth::cable_pairing::CablePairingHost {
                address: host.info().address,
                adapter_index: None,
            }),
        cable_ble_host,
        controller_store.clone(),
        cable_registry_error,
    );
    let mut listen_inbound = true;
    let _ = events.send(Event::Log(
        "Inbound reconnects enabled for known bonded controllers".into(),
    ));
    loop {
        tokio::select! {
            Some(result) = cable_pairing.receive() => {
                if let Ok(crate::cable_pairing::Outcome::Paired { device, state }) = &result {
                    if device.wireless_transport() == TransportKind::BrEdr {
                        identities.insert(state.controller_address, BrIdentity::new(Some(device.profile_id.clone())));
                    } else {
                        let addr = state.controller_address;
                        let slot = known_ble.get(&addr).map_or_else(
                            || next_slot(&active, &pending_inbound, &pending_outbound, &pending_ble),
                            |known| known.slot,
                        );
                        let known = KnownBle {
                            adapter_index: cable_ble_host.and_then(|host| host.adapter_index).unwrap_or(0),
                            profile_id: device.profile_id.clone(),
                            device: BleDevice {
                                addr, name: Some(device.name.clone()), rssi: None,
                                services: Vec::new(), manufacturer_data: HashMap::new(),
                            },
                            slot,
                        };
                        known_ble.insert(addr, known.clone());
                        if let Err(error) = save_known_ble_controllers(controller_store.as_ref(), &known_ble) {
                            let _ = events.send(Event::Log(format!("Could not save cable-paired player slot: {error}")));
                        }
                        if !pending_ble.contains_key(&addr) && !active.contains_key(&addr) {
                            pending_ble.insert(addr, spawn_ble_connection(
                                known.adapter_index, known.device, known.slot, true,
                                events.clone(), ble_tx.clone(), ble_scan_gate.clone(),
                            ));
                        }
                    }
                }
                let _ = events.send(Event::CablePairing(crate::cable_pairing::Event::Finished(
                    result.map_err(|error| error.to_string()),
                )));
            }
            Some(recovery) = ble_recovery_rx.recv() => {
                if let Some((_, task)) = ble_recovery_task.take() {
                    let _ = task.await;
                }
                let addr = recovery.device.addr;
                if known_ble.contains_key(&addr) || pending_ble.contains_key(&addr) {
                    continue;
                }
                let slot = next_slot(
                    &active,
                    &pending_inbound,
                    &pending_outbound,
                    &pending_ble,
                );
                let known = KnownBle {
                    adapter_index: recovery.adapter_index,
                    profile_id: controller_for_ble(&recovery.device)
                        .map(|controller| controller.id())
                        .unwrap_or_else(|| ProfileId::new("nintendo.switch2.unknown")),
                    device: recovery.device,
                    slot,
                };
                diagnostic(&format!(
                    "Recovered existing Switch 2 bond for {addr} ({}) in player slot {}",
                    known.label(),
                    slot + 1
                ));
                let _ = events.send(Event::Log(format!(
                    "Recovered bonded {} at {addr}; connecting…",
                    known.label()
                )));
                known_ble.insert(addr, known.clone());
                if let Err(error) = save_known_ble_controllers(controller_store.as_ref(), &known_ble) {
                    let _ = events.send(Event::Log(format!(
                        "Could not persist recovered Switch 2 controller: {error}"
                    )));
                }
                let pending = spawn_ble_connection(
                    known.adapter_index,
                    known.device,
                    known.slot,
                    true,
                    events.clone(),
                    ble_tx.clone(),
                    ble_scan_gate.clone(),
                );
                pending_ble.insert(addr, pending);
            }
            Some(completion) = ble_rx.recv() => {
                let is_current = pending_ble
                    .get(&completion.addr)
                    .is_some_and(|pending| pending.slot == completion.slot);
                if !is_current {
                    if let Ok(mut connection) = completion.result {
                        let _ = connection.transport.disconnect().await;
                    }
                    continue;
                }
                pending_ble.remove(&completion.addr);
                match completion.result {
                    Ok(BleConnection {
                        transport,
                        mut notifications,
                        mut disconnects,
                        session,
                    }) => {
                        known_ble.insert(
                            completion.addr,
                            KnownBle {
                                adapter_index: completion.adapter_index,
                                device: completion.device.clone(),
                                profile_id: completion.profile_id.clone(),
                                slot: completion.slot,
                            },
                        );
                        if let Err(error) = save_known_ble_controllers(controller_store.as_ref(), &known_ble) {
                            let _ = events.send(Event::Log(format!(
                                "Could not persist bonded Switch 2 controller: {error}"
                            )));
                        }
                        stop_active(&mut active, completion.addr).await;
                        let report_events = events.clone();
                        let disconnect_tx = ble_disconnect_tx.clone();
                        let addr = completion.addr;
                        let slot = completion.slot;
                        let mut report_decoder = decoder::for_profile(&completion.profile_id);
                        let report_task = tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    result = notifications.recv() => match result {
                                        Ok(notification) => {
                                            if let Some(input) = report_decoder
                                                .as_mut()
                                                .and_then(|decoder| decoder.decode(
                                                    &notification.value,
                                                    notification.received_at,
                                                ))
                                            {
                                                let _ = report_events.send(Event::DecodedInput {
                                                    addr: addr.to_string(),
                                                    input,
                                                });
                                            }
                                            let _ = report_events.send(Event::BleReport {
                                                addr: addr.to_string(),
                                                characteristic: notification.uuid.to_string(),
                                                bytes: notification.value.to_vec(),
                                                received_at: notification.received_at,
                                            });
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                            let _ = report_events.send(Event::BleReportsDropped {
                                                addr: addr.to_string(),
                                                count,
                                            });
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                    },
                                    result = disconnects.recv() => {
                                        if result.is_ok() {
                                            let _ = disconnect_tx.send(BleDisconnected { addr, slot });
                                        }
                                        break;
                                    }
                                }
                            }
                        });
                        active.insert(completion.addr, ActiveController {
                            transport: ActiveTransport::Ble(Box::new(transport)),
                            report_task,
                            identity: BrIdentity::new(Some(completion.profile_id.clone())),
                            slot: completion.slot,
                            session: Some(session),
                        });
                        diagnostic(&format!(
                            "Switch 2 GATT notifications and wake reconnect ready for {} in player slot {}",
                            completion.addr,
                            completion.slot + 1
                        ));
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: ControllerRegistry::standard()
                                .get(&completion.profile_id)
                                .map_or("Switch 2 controller", |controller| controller.display_name())
                                .into(),
                            radio: RadioKind::Ble,
                            slot: completion.slot,
                        });
                    }
                    Err(error) => {
                        diagnostic(&format!(
                            "Switch 2 BLE connection failed for {}: {error}",
                            completion.addr
                        ));
                        let message = format!("{} BLE connection failed: {error}", completion.addr);
                        if completion.automatic {
                            let _ = events.send(Event::Log(message));
                        } else {
                            let _ = events.send(Event::Error(message));
                        }
                    }
                }
                if let Some(adapter_index) = controller_order_adapter {
                    start_controller_order_scans(
                        adapter_index,
                        (
                            pending_inbound.is_empty() && pending_outbound.is_empty(),
                            pending_ble.is_empty() && ble_adapter_count > 0,
                        ),
                        host.as_ref(),
                        &events,
                        &ble_scan_gate,
                        &mut br_scan_task,
                        &mut ble_scan_task,
                    );
                }
            }
            Some(disconnected) = ble_disconnect_rx.recv() => {
                let is_current = active
                    .get(&disconnected.addr)
                    .is_some_and(|controller| {
                        controller.slot == disconnected.slot
                            && matches!(&controller.transport, ActiveTransport::Ble(_))
                    });
                if !is_current {
                    continue;
                }
                stop_active(&mut active, disconnected.addr).await;
                let _ = events.send(Event::Disconnected {
                    addr: disconnected.addr.to_string(),
                    reason: "Switch 2 controller asleep; waiting for a wake advertisement…".into(),
                });
                let Some(known) = known_ble.get(&disconnected.addr).cloned() else {
                    continue;
                };
                if pending_ble.contains_key(&disconnected.addr) {
                    continue;
                }
                diagnostic(&format!(
                    "Waiting for wake advertisement from Switch 2 controller {}",
                    disconnected.addr
                ));
                let _ = events.send(Event::Log(format!(
                    "Wake reconnect armed for {}; press a controller button",
                    disconnected.addr
                )));
                let pending = spawn_ble_connection(
                    known.adapter_index,
                    known.device,
                    known.slot,
                    true,
                    events.clone(),
                    ble_tx.clone(),
                    ble_scan_gate.clone(),
                );
                pending_ble.insert(disconnected.addr, pending);
            }
            Some(completion) = outbound_rx.recv() => {
                let is_current = pending_outbound
                    .get(&completion.addr)
                    .is_some_and(|pending| pending.slot == completion.slot);
                if !is_current {
                    if let Ok(mut connection) = completion.result {
                        let _ = connection.transport.disconnect().await;
                    }
                    continue;
                }
                pending_outbound.remove(&completion.addr);
                match completion.result {
                    Ok(BrConnection {
                        transport,
                        reports,
                        session,
                    }) => {
                        listen_inbound = true;
                        if completion.identity.token().is_some() {
                            identities.insert(completion.addr, completion.identity.clone());
                            if let Err(error) = save_controller_identities(controller_store.as_ref(), &identities) {
                                let _ = events.send(Event::Log(format!(
                                    "Could not persist controller identity: {error}"
                                )));
                            }
                        }
                        stop_active(&mut active, completion.addr).await;
                        let report_task = spawn_bredr_report_task(
                            reports,
                            events.clone(),
                            completion.addr,
                            completion.identity.clone(),
                            session.as_ref().and_then(|session| session.ds3_motion()),
                            session.as_ref().and_then(|session| session.sensor_calibration()),
                        );
                        active.insert(
                            completion.addr,
                            ActiveController {
                                transport: ActiveTransport::BrEdr(transport),
                                report_task,
                                identity: completion.identity.clone(),
                                slot: completion.slot,
                                session,
                            },
                        );
                        diagnostic(&format!(
                            "BR/EDR HID channels connected for {} in player slot {}",
                            completion.addr,
                            completion.slot + 1
                        ));
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: completion.identity.label().into(),
                            radio: RadioKind::BrEdr,
                            slot: completion.slot,
                        });
                    }
                    Err(error) => {
                        diagnostic(&format!(
                            "BR/EDR connection failed for {}: {error}",
                            completion.addr
                        ));
                        let _ = events.send(Event::Error(format!(
                            "{} connection failed: {error}",
                            completion.addr
                        )));
                    }
                }
                if let Some(adapter_index) = controller_order_adapter {
                    start_controller_order_scans(
                        adapter_index,
                        (
                            pending_inbound.is_empty() && pending_outbound.is_empty(),
                            pending_ble.is_empty() && ble_adapter_count > 0,
                        ),
                        host.as_ref(),
                        &events,
                        &ble_scan_gate,
                        &mut br_scan_task,
                        &mut ble_scan_task,
                    );
                }
            }
            Some(completion) = inbound_rx.recv() => {
                let is_current = pending_inbound
                    .get(&completion.addr)
                    .is_some_and(|pending| pending.handle == completion.handle);
                if !is_current {
                    if let Ok(mut connection) = completion.result {
                        let _ = connection.transport.disconnect().await;
                    }
                    continue;
                }
                pending_inbound.remove(&completion.addr);
                match completion.result {
                    Ok(BrConnection {
                        transport,
                        reports,
                        session,
                    }) => {
                        stop_active(&mut active, completion.addr).await;
                        let report_task = spawn_bredr_report_task(
                            reports,
                            events.clone(),
                            completion.addr,
                            completion.identity.clone(),
                            session.as_ref().and_then(|session| session.ds3_motion()),
                            session.as_ref().and_then(|session| session.sensor_calibration()),
                        );
                        active.insert(
                            completion.addr,
                            ActiveController {
                                transport: ActiveTransport::BrEdr(transport),
                                report_task,
                                identity: completion.identity.clone(),
                                slot: completion.slot,
                                session,
                            },
                        );
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: completion.identity.label().into(),
                            radio: RadioKind::BrEdr,
                            slot: completion.slot,
                        });
                    }
                    Err(error) => {
                        if completion.identity.is_switch() {
                            let _ = events.send(Event::Log(
                                "Nintendo reconnect failed; accepting its next automatic page retry"
                                    .into(),
                            ));
                        }
                        let _ = events.send(Event::Error(format!(
                            "{} reconnect failed: {error}",
                            completion.addr
                        )));
                    }
                }
                if let Some(adapter_index) = controller_order_adapter {
                    start_controller_order_scans(
                        adapter_index,
                        (
                            pending_inbound.is_empty() && pending_outbound.is_empty(),
                            pending_ble.is_empty() && ble_adapter_count > 0,
                        ),
                        host.as_ref(),
                        &events,
                        &ble_scan_gate,
                        &mut br_scan_task,
                        &mut ble_scan_task,
                    );
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break; };
                match command {
                    Command::CablePairing(action) => cable_pairing.start(action),
                    Command::BeginControllerOrder { adapter_index } => {
                        controller_order_adapter = Some(adapter_index);
                        listen_inbound = true;
                        stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
                        stop_ble_scan(&mut ble_scan_task, &events).await;
                        stop_ble_recovery(&mut ble_recovery_task).await;

                        for (_, pending) in pending_ble.drain() {
                            pending.task.abort();
                            let _ = pending.task.await;
                        }
                        if ble_adapter_count > 0 {
                            let _ = stop_ble_adapter_scan(adapter_index).await;
                        }
                        for (_, pending) in pending_outbound.drain() {
                            pending.task.abort();
                            let _ = pending.task.await;
                        }
                        for (_, pending) in pending_inbound.drain() {
                            pending.task.abort();
                            let _ = pending.task.await;
                            if let (Some(host), Some(link)) = (&host, pending.link) {
                                let _ = host.disconnect_link(pending.handle, link).await;
                            }
                        }
                        sleep_all_for_controller_order(&mut active, &events).await;

                        start_controller_order_scans(
                            adapter_index,
                            (true, ble_adapter_count > 0),
                            host.as_ref(),
                            &events,
                            &ble_scan_gate,
                            &mut br_scan_task,
                            &mut ble_scan_task,
                        );
                        let _ = events.send(Event::Idle(
                            "Controller order mode ready; wake or pair controllers in player order"
                                .into(),
                        ));
                    }
                    Command::EndControllerOrder => {
                        controller_order_adapter = None;
                        stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
                        stop_ble_scan(&mut ble_scan_task, &events).await;
                        for known in known_ble.values().cloned().collect::<Vec<_>>() {
                            let addr = known.device.addr;
                            if active.contains_key(&addr) || pending_ble.contains_key(&addr) {
                                continue;
                            }
                            let pending = spawn_ble_connection(
                                known.adapter_index,
                                known.device,
                                known.slot,
                                true,
                                events.clone(),
                                ble_tx.clone(),
                                ble_scan_gate.clone(),
                            );
                            pending_ble.insert(addr, pending);
                        }
                        let _ = events.send(Event::Idle(
                            "Controller order saved; bonded wake reconnects remain armed".into(),
                        ));
                    }
                    Command::ConnectBr {
                        device,
                        pin,
                        mut profile_id,
                    } => {
                        let (Some(host), Some(pairing)) = (&host, &pairing) else {
                            let _ = events.send(Event::Error("dedicated BR/EDR adapter or pairing store is unavailable".into()));
                            continue;
                        };
                        if pending_outbound.contains_key(&device.addr) {
                            let _ = events.send(Event::Error(format!(
                                "a connection attempt for {} is already running",
                                device.addr
                            )));
                            continue;
                        }
                        stop_br_scan(&mut br_scan_task, Some(host), &events).await;
                        if controller_order_adapter.is_none() {
                            stop_ble_scan(&mut ble_scan_task, &events).await;
                        }
                        if let Some(pending) = pending_inbound.remove(&device.addr) {
                            pending.task.abort();
                            if let Some(link) = pending.link {
                                let _ = host.disconnect_link(pending.handle, link).await;
                            }
                        }
                        stop_active(&mut active, device.addr).await;
                        profile_id = identities.get(&device.addr)
                            .and_then(|identity| identity.profile_id.clone()).or(profile_id);
                        let mut identity = BrIdentity::new(profile_id);
                        if identity.is_switch() || identity.is_wii_remote() {
                            if let Err(error) = pairing.forget(device.addr) {
                                let _ = events.send(Event::Error(format!(
                                    "could not clear the previous Nintendo bond: {error}"
                                )));
                                continue;
                            }
                            diagnostic(&format!(
                                "Forcing fresh pairing for Nintendo controller {}",
                                device.addr
                            ));
                            let _ = events.send(Event::Log(
                                "Cleared the previous Nintendo bond for fresh pairing".into(),
                            ));
                        }
                        if let Err(error) = configure_pin(pairing, device.addr, pin, &identity) {
                            let _ = events.send(Event::Error(error.to_string()));
                            continue;
                        }
                        let pin_source = match pin {
                            BondingMode::Ssp if identity.is_switch() => {
                                "SSP Just Works; legacy fallback PIN = 0000".to_owned()
                            }
                            BondingMode::Ssp => "SSP / no legacy PIN".to_owned(),
                            BondingMode::WiiOneTwo => {
                                format!("temporary PIN = reverse of Wii address {}", device.addr)
                            }
                            BondingMode::WiiSync => format!(
                                "bonding PIN = reverse of host address {}",
                                host.info().address
                            ),
                        };
                        let _ = events.send(Event::Log(format!(
                            "Pairing policy for {}: {pin_source}",
                            device.addr
                        )));
                        diagnostic(&format!("Pairing policy for {}: {pin_source}", device.addr));
                        let _ = events.send(Event::Busy(format!("Connecting and authenticating {}…", device.addr)));
                        let mut transport =
                            BrEdrHid::new(host.clone(), device.addr, link_keys.clone());
                        if identity.is_wii_remote() {
                            // Keep the controller's default retransmission policy until
                            // the four-controller latency value is tuned on Wii hardware.
                            transport.set_automatic_flush_slots(0);
                        }
                        let reports = transport.reports();
                        let slot = next_slot(
                            &active,
                            &pending_inbound,
                            &pending_outbound,
                            &pending_ble,
                        );
                        let addr = device.addr;
                        let completion_tx = outbound_tx.clone();
                        let task_events = events.clone();
                        let task_host = host.clone();
                        let task = tokio::spawn(async move {
                            let result = async {
                                // During fresh SSP pairing the host opens both HID
                                // PSMs. Bonded button-wake reconnects use the opposite,
                                // controller-initiated direction handled below.
                                identity.establish(&task_host, &device, &mut transport).await?;

                                let session = match identity.connect(&mut transport, slot).await {
                                    Ok(session) => session,
                                    Err(error) => {
                                        let _ = transport.disconnect().await;
                                        return Err(error);
                                    }
                                };
                                if identity.is_wii_remote() {
                                    diagnostic(&format!(
                                        "Initialized Wii Remote {addr}: player LED {}, continuous buttons+accelerometer+IR+MotionPlus reports",
                                        slot + 1
                                    ));
                                    let _ = task_events.send(Event::Log(format!(
                                        "Wii Remote player LED {}, IR camera, MotionPlus, and continuous input enabled",
                                        slot + 1
                                    )));
                                } else if identity.is_switch() {
                                    diagnostic(&format!(
                                        "Initialized {} {addr}: native 0x30 reports, vibration, player LED {}",
                                        identity.label(),
                                        slot + 1
                                    ));
                                    let _ = task_events.send(Event::Log(format!(
                                        "{} native input enabled in player slot {}",
                                        identity.label(),
                                        slot + 1
                                    )));
                                }
                                Ok(BrConnection {
                                    transport,
                                    reports,
                                    session,
                                })
                            }
                            .await;
                            let _ = completion_tx.send(OutboundResult {
                                addr,
                                identity,
                                slot,
                                result,
                            });
                        });
                        pending_outbound.insert(addr, PendingOutbound { slot, task });
                    }
                    Command::ConnectBle { adapter_index, device } => {
                        let Some(protocol) = controller_for_ble(&device).filter(|controller| {
                            controller.id().as_str().starts_with("nintendo.switch2.")
                        }) else {
                            let _ = events.send(Event::Error(format!(
                                "{} is not a supported Switch 2 controller",
                                device.addr
                            )));
                            continue;
                        };
                        if pending_ble.contains_key(&device.addr) {
                            let _ = events.send(Event::Error(format!(
                                "a BLE connection attempt for {} is already running",
                                device.addr
                            )));
                            continue;
                        }
                        stop_ble_recovery(&mut ble_recovery_task).await;
                        if controller_order_adapter.is_none() {
                            stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
                        }
                        stop_ble_scan(&mut ble_scan_task, &events).await;
                        let addr = device.addr;
                        stop_active(&mut active, addr).await;
                        let slot = next_slot(
                            &active,
                            &pending_inbound,
                            &pending_outbound,
                            &pending_ble,
                        );
                        let pairing_mode = !protocol.accepts_reconnect_advertisement(
                            &ble_device_identity(&device),
                            None,
                        );
                        let automatic = controller_order_adapter.is_none()
                            && known_ble.contains_key(&addr)
                            && !pairing_mode;
                        let _ = events.send(Event::Busy(format!("Connecting to {} over BLE…", protocol.display_name())));
                        diagnostic(&format!(
                            "Starting Switch 2 BLE connection to {addr} ({}) on OS adapter {adapter_index}",
                            protocol.display_name()
                        ));
                        let pending = spawn_ble_connection(
                            adapter_index,
                            device,
                            slot,
                            automatic,
                            events.clone(),
                            ble_tx.clone(),
                            ble_scan_gate.clone(),
                        );
                        pending_ble.insert(addr, pending);
                    }
                    Command::SelectSwitch2InputReport { addr, report_id } => {
                        let Some(known) = known_ble.get(&addr) else {
                            let _ = events.send(Event::Error(format!(
                                "no known Switch 2 controller at {addr}"
                            )));
                            continue;
                        };
                        let Some(protocol) = known.controller() else {
                            let _ = events.send(Event::Error(format!(
                                "controller profile {} is unavailable",
                                known.profile_id
                            )));
                            continue;
                        };
                        let Some(controller) = active.get_mut(&addr) else {
                            let _ = events.send(Event::Error(format!(
                                "no connected Switch 2 controller at {addr}"
                            )));
                            continue;
                        };
                        let ActiveTransport::Ble(transport) = &mut controller.transport else {
                            let _ = events.send(Event::Error(format!(
                                "controller {addr} is not connected over BLE"
                            )));
                            continue;
                        };
                        let identity = ble_device_identity(&known.device);
                        let Some(profile) = protocol.gatt_profile(&identity).ok().flatten() else {
                            let _ = events.send(Event::Error(format!(
                                "{} does not expose a GATT input profile",
                                protocol.display_name()
                            )));
                            continue;
                        };
                        let input_uuid = if report_id == 0x05 {
                            COMMON_INPUT_UUID
                        } else if Some(report_id) == protocol.preferred_input_report_id() {
                            profile.input
                        } else {
                            let _ = events.send(Event::Error(format!(
                                "input report 0x{report_id:02X} does not belong to {}",
                                protocol.display_name()
                            )));
                            continue;
                        };
                        match transport.select_input_characteristic(input_uuid).await {
                            Ok(()) => {
                                let _ = events.send(Event::Switch2InputReportSelected {
                                    addr: addr.to_string(),
                                    report_id,
                                });
                                let _ = events.send(Event::Log(format!(
                                    "{addr}: selected Switch 2 input report 0x{report_id:02X}"
                                )));
                            }
                            Err(error) => {
                                let _ = events.send(Event::Switch2InputReportSelectionFailed {
                                    addr: addr.to_string(),
                                    report_id,
                                    reason: error.to_string(),
                                });
                                let _ = events.send(Event::Error(format!(
                                    "could not select input report 0x{report_id:02X} for {addr}: {error}"
                                )));
                            }
                        }
                    }
                    Command::SubscribeAllSwitch2InputReports { addr } => {
                        let Some(known) = known_ble.get(&addr) else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: "controller is not known".into(),
                            });
                            continue;
                        };
                        let Some(protocol) = known.controller() else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: format!("controller profile {} is unavailable", known.profile_id),
                            });
                            continue;
                        };
                        let Some(native_report_id) = protocol.preferred_input_report_id() else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: format!("{} has no native input report", protocol.display_name()),
                            });
                            continue;
                        };
                        let Some(controller) = active.get_mut(&addr) else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: "controller is not connected".into(),
                            });
                            continue;
                        };
                        let ActiveTransport::Ble(transport) = &mut controller.transport else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: "controller is not connected over BLE".into(),
                            });
                            continue;
                        };
                        match transport.subscribe_all_input_characteristics().await {
                            Ok(uuids) => {
                                let report_ids = uuids
                                    .into_iter()
                                    .filter_map(|uuid| {
                                        (uuid == COMMON_INPUT_UUID)
                                            .then_some(0x05)
                                            .or_else(|| (uuid != COMMON_INPUT_UUID).then_some(native_report_id))
                                    })
                                    .collect::<Vec<_>>();
                                let _ = events.send(Event::Switch2InputReportsSubscribed {
                                    addr: addr.to_string(),
                                    report_ids: report_ids.clone(),
                                });
                                let reports = report_ids
                                    .iter()
                                    .map(|report_id| format!("0x{report_id:02X}"))
                                    .collect::<Vec<_>>()
                                    .join(" and ");
                                let _ = events.send(Event::Log(format!(
                                    "{addr}: subscribed Switch 2 input reports {reports}"
                                )));
                            }
                            Err(error) => {
                                let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                    addr: addr.to_string(),
                                    reason: error.to_string(),
                                });
                            }
                        }
                    }
                    Command::ListenInbound(enabled) => {
                        listen_inbound = enabled;
                        let _ = events.send(Event::Log(if enabled { "Inbound ACL acceptance enabled".into() } else { "Inbound ACL acceptance disabled".into() }));
                    }
                    Command::Forget(addr) => {
                        if let Some(pending) = pending_inbound.remove(&addr) {
                            pending.task.abort();
                            if let (Some(host), Some(link)) = (&host, pending.link) {
                                let _ = host.disconnect_link(pending.handle, link).await;
                            }
                        }
                        stop_active(&mut active, addr).await;
                        let Some(pairing) = &pairing else {
                            let _ = events.send(Event::Error("BR/EDR pairing store is unavailable".into()));
                            continue;
                        };
                        match pairing.forget(addr) {
                            Ok(()) => {
                                identities.remove(&addr);
                                if let Err(error) = save_controller_identities(controller_store.as_ref(), &identities) {
                                    let _ = events.send(Event::Log(format!("Could not update controller identities: {error}")));
                                }
                                let _ = events.send(Event::Idle(format!("Forgot link key and controller identity for {addr}")));
                            }
                            Err(error) => { let _ = events.send(Event::Error(error.to_string())); }
                        }
                    }
                    Command::SendOutput { addr, bytes } => {
                        match active.get_mut(&addr) {
                            Some(active) => match active.transport.send(&bytes).await {
                                Ok(()) => { let _ = events.send(Event::Log(format!("Sent {} output bytes", bytes.len()))); }
                                Err(error) => { let _ = events.send(Event::Error(error.to_string())); }
                            },
                            None => { let _ = events.send(Event::Error(format!("no connected HID controller at {addr}"))); }
                        }
                    }
                    Command::SleepBle(addr) => {
                        let Some(known) = known_ble.get(&addr).cloned() else {
                            let _ = events.send(Event::Error(format!(
                                "no bonded Switch 2 wake metadata for {addr}"
                            )));
                            continue;
                        };
                        let Some(mut controller) = active.remove(&addr) else {
                            let _ = events.send(Event::Error(format!(
                                "no connected Switch 2 controller at {addr}"
                            )));
                            continue;
                        };
                        let sleep_result = if matches!(&controller.transport, ActiveTransport::Ble(_)) {
                            controller.sleep().await
                        } else {
                            Err(polymer_bluetooth::Error::Unsupported(
                                "Switch 2 sleep is only available for BLE sessions",
                            ))
                        };
                        if let Err(error) = sleep_result {
                            active.insert(addr, controller);
                            let _ = events.send(Event::Error(format!(
                                "could not put Switch 2 controller {addr} to sleep: {error}"
                            )));
                            continue;
                        }

                        diagnostic(&format!(
                            "Sent bonded sleep command to Switch 2 controller {addr}"
                        ));
                        controller.report_task.abort();
                        // Let WinRT submit the write before closing our local
                        // GATT session. The command itself has no response.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let _ = controller.transport.disconnect().await;
                        let _ = events.send(Event::Disconnected {
                            addr: addr.to_string(),
                            reason: "Switch 2 controller sleeping; wake reconnect is armed".into(),
                        });
                        let _ = events.send(Event::Log(format!(
                            "Wake reconnect armed for {addr}; press HOME or another controller button"
                        )));
                        let pending = spawn_ble_connection(
                            known.adapter_index,
                            known.device,
                            known.slot,
                            true,
                            events.clone(),
                            ble_tx.clone(),
                            ble_scan_gate.clone(),
                        );
                        pending_ble.insert(addr, pending);
                    }
                    Command::Disconnect(addr) => {
                        let targets = match addr {
                            Some(addr) => vec![addr],
                            None => active.keys().copied().collect(),
                        };
                        for addr in targets {
                            disconnect_active(&mut active, addr, &events).await;
                        }
                    }
                    Command::Shutdown => break,
                }
            }
            event = receive_hci(&mut hci_events) => {
                let Some(event) = event else { continue; };
                let event = match event {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => { let _ = events.send(Event::Log(format!("HCI monitor lagged by {count} events"))); continue; }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                match event {
                    HciEvent::ConnectionRequest { addr, .. } if listen_inbound => {
                        let Some(host) = &host else { continue; };
                        let Some(identity) = identities.get(&addr).cloned() else {
                            diagnostic(&format!("Rejecting inbound connection from unknown address {addr}"));
                            let _ = host.reject_connection(addr).await;
                            continue;
                        };
                        stop_br_scan(&mut br_scan_task, Some(host), &events).await;
                        if pending_inbound.contains_key(&addr)
                            || pending_outbound.contains_key(&addr)
                        {
                            diagnostic(&format!(
                                "Rejecting duplicate inbound connection while {addr} is already attaching"
                            ));
                            let _ = host.reject_connection(addr).await;
                            continue;
                        }
                        stop_active(&mut active, addr).await;

                        let _ = events.send(Event::Busy(format!("Accepting inbound connection from {addr}…")));
                        // Subscribe before accepting the ACL. Nintendo sends an
                        // L2CAP Information Request immediately after Connection
                        // Complete, before a newly constructed HID transport can
                        // otherwise subscribe to the frame broadcast.
                        let early_frames = host.l2cap_frames();
                        let early_hci_events = host.events();
                        let become_master = identity.policy(addr, 0).become_master;
                        match host.accept_connection_with_role(addr, become_master).await {
                            Ok(handle) => {
                                let link = host.link(handle);
                                let mut transport =
                                    BrEdrHid::new(host.clone(), addr, link_keys.clone());
                                if identity.is_wii_remote() {
                                    transport.set_automatic_flush_slots(0);
                                }
                                let reports = transport.reports();
                                let slot = next_slot(
                                    &active,
                                    &pending_inbound,
                                    &pending_outbound,
                                    &pending_ble,
                                );
                                let completion_tx = inbound_tx.clone();
                                let task_events = events.clone();
                                let task_host = host.clone();
                                let task = tokio::spawn(async move {
                                    let result = async {
                                        let policy = identity.policy(addr, slot);
                                        if policy.host_opens_hid_on_reconnect {
                                            transport.attach_inbound_host_hid_with_frames(handle, early_frames, early_hci_events).await?;
                                        } else if policy.wait_for_encryption_before_inbound_hid {
                                            transport
                                                .attach_inbound_encrypted_with_frames(
                                                    handle,
                                                    early_frames,
                                                    early_hci_events,
                                                )
                                                .await?;
                                        } else {
                                            transport
                                                .attach_inbound_with_frames(handle, early_frames, early_hci_events)
                                                .await?;
                                        }

                                        let session = identity.connect(&mut transport, slot).await?;
                                        if identity.is_wii_remote() {
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected Wii Remote in player slot {}",
                                                slot + 1
                                            )));
                                        } else if identity.is_switch() {
                                            diagnostic(&format!(
                                                "Reconnected {} {addr}: native 0x30 reports and player LED {}",
                                                identity.label(),
                                                slot + 1
                                            ));
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected {} in player slot {} with native input",
                                                identity.label(),
                                                slot + 1
                                            )));
                                        } else if identity.token() == Some("sony.dualshock3") {
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected DualShock 3 in player slot {}; Bluetooth input activated",
                                                slot + 1
                                            )));
                                        }
                                        Ok(BrConnection {
                                            transport,
                                            reports,
                                            session,
                                        })
                                    }
                                    .await;
                                    if let (Err(_), Some(link)) = (&result, link) {
                                        let _ = task_host.disconnect_link(handle, link).await;
                                    }
                                    let _ = completion_tx.send(InboundResult {
                                        addr,
                                        handle,
                                        identity,
                                        slot,
                                        result,
                                    });
                                });
                                pending_inbound.insert(addr, PendingInbound { handle, link, slot, task });
                            }
                            Err(error) => { let _ = events.send(Event::Error(error.to_string())); }
                        }
                    }
                    HciEvent::PinCodeRequest { addr } => {
                        diagnostic(&format!("PIN requested by {addr}"));
                        let _ = events.send(Event::Log(format!("PIN requested by {addr}")));
                    }
                    HciEvent::LinkKeyRequest { addr } => {
                        diagnostic(&format!("Link key requested by {addr}"));
                        let _ = events.send(Event::Log(format!("Link key requested by {addr}")));
                    }
                    HciEvent::IoCapabilityRequest { addr } => { let _ = events.send(Event::Log(format!("SSP capability requested by {addr}"))); }
                    HciEvent::LinkKeyNotification { addr, .. } => {
                        diagnostic(&format!("Stored new link key for {addr}"));
                        let _ = events.send(Event::Log(format!("Stored new link key for {addr}")));
                    }
                    HciEvent::AuthenticationComplete { status, handle } => {
                        diagnostic(&format!("Authentication handle {handle}: status 0x{status:02X}"));
                        let _ = events.send(Event::Log(format!("Authentication handle {handle}: status 0x{status:02X}")));
                    }
                    HciEvent::EncryptionChange { status, handle, enabled } => {
                        diagnostic(&format!("Encryption handle {handle}: {enabled}, status 0x{status:02X}"));
                        let _ = events.send(Event::Log(format!("Encryption handle {handle}: {enabled}, status 0x{status:02X}")));
                    }
                    HciEvent::EncryptionKeyRefreshComplete { status, handle } => {
                        diagnostic(&format!("Encryption key refresh handle {handle}: status 0x{status:02X}"));
                        let _ = events.send(Event::Log(format!("Encryption key refresh handle {handle}: status 0x{status:02X}")));
                    }
                    HciEvent::DisconnectionComplete { status, handle, reason } => {
                        let active_addr = active.iter().find_map(|(addr, controller)| {
                            (controller.transport.bredr_handle() == Some(handle)).then_some(*addr)
                        });
                        let pending_addr = pending_inbound.iter().find_map(|(addr, pending)| {
                            (pending.handle == handle).then_some(*addr)
                        });
                        diagnostic(&format!(
                            "HCI disconnection complete: status 0x{status:02X}, handle {handle}, reason 0x{reason:02X}, active address {active_addr:?}, pending address {pending_addr:?}"
                        ));
                        if let Some(addr) = pending_addr
                            && let Some(pending) = pending_inbound.remove(&addr)
                        {
                            pending.task.abort();
                            let _ = events.send(Event::Log(format!(
                                "{addr} disconnected during HID setup (reason 0x{reason:02X}); ready for its next page retry"
                            )));
                        }
                        if let Some(addr) = active_addr
                            && let Some(controller) = active.remove(&addr)
                        {
                            controller.report_task.abort();
                            let _ = events.send(Event::Disconnected {
                                addr: addr.to_string(),
                                reason: format!("Controller disconnected (reason 0x{reason:02X})"),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
    stop_ble_scan(&mut ble_scan_task, &events).await;
    stop_ble_recovery(&mut ble_recovery_task).await;
    for (_, pending) in pending_inbound.drain() {
        pending.task.abort();
        if let (Some(host), Some(link)) = (&host, pending.link) {
            let _ = host.disconnect_link(pending.handle, link).await;
        }
    }
    for (_, pending) in pending_outbound.drain() {
        pending.task.abort();
    }
    for (_, pending) in pending_ble.drain() {
        pending.task.abort();
    }
    stop_all_active(&mut active).await;
    if let Some(task) = pairing_task {
        task.abort();
    }
}

async fn receive_hci(
    receiver: &mut Option<tokio::sync::broadcast::Receiver<HciEvent>>,
) -> Option<std::result::Result<HciEvent, tokio::sync::broadcast::error::RecvError>> {
    match receiver {
        Some(receiver) => Some(receiver.recv().await),
        None => std::future::pending().await,
    }
}
