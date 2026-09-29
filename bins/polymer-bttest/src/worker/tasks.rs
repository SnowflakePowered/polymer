use super::*;

pub(super) fn spawn_bredr_report_task(
    mut reports: ReportReceiver,
    events: mpsc::Sender<Event>,
    addr: DeviceAddr,
    identity: BrIdentity,
    motion: Option<polymer_bluetooth::devices::sony::dualshock3::motion::MotionState>,
    sensor_calibration: Option<polymer_bluetooth::devices::SensorCalibration>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut wiimote_decoder = wiimote::WiimoteTelemetryDecoder;
        let mut report_decoder = identity.profile_id.as_ref().and_then(decoder::for_profile);
        if let Some(decoder) = &mut report_decoder {
            decoder.set_ds3_motion(motion.clone());
            decoder.set_sensor_calibration(sensor_calibration);
        }
        let diagnostic_deadline = Instant::now() + Duration::from_secs(60);
        let mut next_diagnostic = Instant::now();
        loop {
            match reports.recv().await {
                Ok(report) => {
                    let received_at = Instant::now();
                    if identity.token() == Some("sony.dualshock3")
                        && received_at >= next_diagnostic
                        && received_at < diagnostic_deadline
                    {
                        diagnostic(&format!(
                            "DS3 native input {addr}: {:02X?}",
                            report.as_ref()
                        ));
                        diagnostic(&format!(
                            "DS3 motion calibration: {:?}",
                            motion.as_ref().and_then(|state| state.snapshot())
                        ));
                        next_diagnostic = received_at + Duration::from_millis(250);
                    }
                    if identity.is_wii_remote()
                        && let Some(telemetry) = wiimote_decoder.process(&report)
                    {
                        let _ = events.send(Event::WiiTelemetry {
                            addr: addr.to_string(),
                            telemetry,
                            received_at,
                        });
                    }
                    if let Some(input) = report_decoder
                        .as_mut()
                        .and_then(|decoder| decoder.decode(&report, received_at))
                    {
                        let _ = events.send(Event::DecodedInput {
                            addr: addr.to_string(),
                            input,
                        });
                    }
                    let _ = events.send(Event::Report {
                        addr: addr.to_string(),
                        bytes: report.to_vec(),
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    let _ = events.send(Event::Log(format!("Dropped {count} raw reports")));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

pub(super) fn spawn_ble_connection(
    adapter_index: usize,
    device: BleDevice,
    slot: usize,
    automatic: bool,
    events: mpsc::Sender<Event>,
    completion_tx: tokio_mpsc::UnboundedSender<BleResult>,
    scan_gate: Arc<Mutex<()>>,
) -> PendingBle {
    let addr = device.addr;
    let identity = ble_device_identity(&device);
    let protocol = controller_for_ble(&device)
        .filter(|controller| controller.id().as_str().starts_with("nintendo.switch2."))
        .unwrap_or_else(|| Arc::new(UnknownSwitch2Controller::default()));
    let profile_id = protocol.id();
    let pairing_mode = !automatic && !protocol.accepts_reconnect_advertisement(&identity, None);
    let result_device = device.clone();
    let task = tokio::spawn(async move {
        let profile = protocol.gatt_profile(&identity).and_then(|profile| {
            profile.ok_or(polymer_bluetooth::Error::Unsupported(
                "controller does not expose a GATT profile",
            ))
        });
        let mut session = protocol.connect(&connect_options(slot));
        let result = async {
        let mut transport = BleGatt::new(adapter_index, profile?);
        if automatic {
            // Rotate the single adapter fairly between dormant controllers.
            // A Switch 2 wake advertisement persists across several windows.
            transport.set_scan_timeout(Duration::from_secs(3));
        }
        let notifications = transport.gatt_notifications();
        let disconnects = transport.disconnections();
        loop {
            let connection = {
                let _scan_guard = scan_gate.lock().await;
                transport.connect(addr).await
            };
            match connection {
                Ok(()) => {
                    if pairing_mode {
                        let bond_result = async {
                            let host_address = default_adapter_address().await?;
                            protocol
                                .bond(&mut transport, &identity, host_address)
                                .await?;
                            Ok::<_, polymer_bluetooth::Error>(host_address)
                        }
                        .await;
                        match bond_result {
                            Ok(host_address) => {
                                diagnostic(&format!(
                                    "Committed Switch 2 bond for {addr} to OS adapter {}",
                                    host_address
                                ));
                                let _ = events.send(Event::Log(format!(
                                    "Bonded {addr} to {}; normal-button wake is enabled",
                                    host_address
                                )));
                            }
                            Err(error) => {
                                let _ = transport.disconnect().await;
                                break Err(error);
                            }
                        }
                    }
                    if let Err(error) = session.synchronize(&mut transport).await {
                        let _ = transport.disconnect().await;
                        break Err(error);
                    }
                    if let Err(error) = session.set_pairing_indicator(&mut transport, false).await {
                        let _ = transport.disconnect().await;
                        break Err(error);
                    }
                    diagnostic(&format!(
                        "{} {addr} acknowledged player LED {} with pairing indicator disabled",
                        protocol.display_name(),
                        slot + 1,
                    ));
                    break Ok(BleConnection {
                        transport,
                        notifications,
                        disconnects,
                        session,
                    });
                }
                Err(error) if !automatic => break Err(error),
                Err(polymer_bluetooth::Error::Unsupported(reason)) => {
                    break Err(polymer_bluetooth::Error::Unsupported(reason));
                }
                Err(polymer_bluetooth::Error::Backoff(delay)) => {
                    tokio::time::sleep(delay.max(Duration::from_millis(100))).await;
                }
                Err(error) if error.is_ble_device_absent() => {
                    // Sleeping is the normal steady state. The next scan waits
                    // for a fresh button-wake advertisement without consuming
                    // a controller cooldown attempt.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => {
                    diagnostic(&format!(
                        "Wake reconnect attempt for {addr} failed: {error}; respecting BLE backoff"
                    ));
                    let _ = events.send(Event::Log(format!(
                        "Wake reconnect for {addr} will retry: {error}"
                    )));
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
        }.await;
        let _ = completion_tx.send(BleResult {
            addr,
            slot,
            adapter_index,
            device: result_device,
            profile_id,
            automatic,
            result,
        });
    });
    PendingBle { slot, task }
}

pub(super) fn spawn_ble_bond_recovery(
    adapter_index: usize,
    host_address: DeviceAddr,
    events: mpsc::Sender<Event>,
    recovery_tx: tokio_mpsc::UnboundedSender<BleRecovery>,
    scan_gate: Arc<Mutex<()>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let result = {
                let _scan_guard = scan_gate.lock().await;
                scan_ble(adapter_index, Duration::from_secs(4)).await
            };
            match result {
                Ok(found) => {
                    if let Some(device) = found.into_iter().find(|device| {
                        controller_for_ble(device).is_some_and(|controller| {
                            controller.id().as_str().starts_with("nintendo.switch2.")
                                && controller.accepts_reconnect_advertisement(
                                    &ble_device_identity(device),
                                    Some(host_address),
                                )
                        })
                    }) {
                        let _ = recovery_tx.send(BleRecovery {
                            adapter_index,
                            device,
                        });
                        break;
                    }
                }
                Err(error) => {
                    diagnostic(&format!(
                        "Existing Switch 2 bond recovery scan failed: {error}"
                    ));
                    let _ = events.send(Event::Log(format!(
                        "Existing Switch 2 bond recovery will retry: {error}"
                    )));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}

pub(super) fn spawn_br_scan(host: Arc<HciHost>, events: mpsc::Sender<Event>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut names: HashMap<DeviceAddr, String> = HashMap::new();
        let mut use_wii_liac = false;
        loop {
            let mode = if use_wii_liac {
                "Wii LIAC"
            } else {
                "general GIAC"
            };
            diagnostic(&format!("Starting BR/EDR inquiry cycle ({mode})"));
            let inquiry = if use_wii_liac {
                host.limited_inquiry(3, 0).await
            } else {
                host.inquiry(3, 0).await
            };
            use_wii_liac = !use_wii_liac;
            match inquiry {
                Ok(found) => {
                    diagnostic(&format!(
                        "BR/EDR inquiry cycle found {} device(s)",
                        found.len()
                    ));
                    // Publish the entire inquiry batch before any remote-name
                    // request. A slow device or cancelled scan must not hide it.
                    for inquiry in &found {
                        if !names.contains_key(&inquiry.addr) {
                            let mut view =
                                device_view(inquiry.clone(), "Resolving device name…".into());
                            // Wait for identification before automatic pairing:
                            // a generic class match cannot choose Nintendo's PIN.
                            view.profile_id = None;
                            view.profile_name = None;
                            diagnostic(&format!(
                                "Discovered BR/EDR address {} (name pending)",
                                inquiry.addr
                            ));
                            let _ = events.send(Event::Device(view));
                        }
                    }
                    for inquiry in found {
                        let name = if let Some(name) = names.get(&inquiry.addr) {
                            name.clone()
                        } else {
                            match host.remote_name(&inquiry).await {
                                Ok(name) if !name.trim().is_empty() => {
                                    diagnostic(&format!("Discovered {} as {name}", inquiry.addr));
                                    names.insert(inquiry.addr, name.clone());
                                    name
                                }
                                Ok(_) => {
                                    diagnostic(&format!(
                                        "Remote-name request for {} returned an empty name; will retry",
                                        inquiry.addr
                                    ));
                                    "Unknown controller".into()
                                }
                                Err(error) => {
                                    diagnostic(&format!(
                                        "Remote-name request for {} failed: {error}; will retry",
                                        inquiry.addr
                                    ));
                                    "Unknown controller".into()
                                }
                            }
                        };
                        let _ = events.send(Event::Device(device_view(inquiry, name)));
                    }
                }
                Err(error) => {
                    diagnostic(&format!("BR/EDR inquiry cycle failed: {error}"));
                    let _ = events.send(Event::Log(format!("BR/EDR inquiry will retry: {error}")));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}

fn device_view(inquiry: InquiryResult, name: String) -> DeviceView {
    let controller = controller_for_bredr(&inquiry, &name);
    DeviceView {
        addr: inquiry.addr.to_string(),
        name,
        rssi: inquiry.rssi,
        class: format!(
            "{:02X}{:02X}{:02X}",
            inquiry.class_of_device[2], inquiry.class_of_device[1], inquiry.class_of_device[0]
        ),
        profile_id: controller.as_ref().map(|controller| controller.id()),
        profile_name: controller.map(|controller| controller.display_name()),
        inquiry,
    }
}

fn controller_for_bredr(inquiry: &InquiryResult, name: &str) -> Option<Arc<dyn ControllerDevice>> {
    let class = inquiry.class_of_device;
    ControllerRegistry::standard()
        .identify(&DeviceIdentity {
            address: inquiry.addr,
            transport: TransportKind::BrEdr,
            name: Some(name.to_owned()),
            class_of_device: Some(
                u32::from(class[0]) | (u32::from(class[1]) << 8) | (u32::from(class[2]) << 16),
            ),
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        })
        .ok()
        .flatten()
}

pub(super) fn spawn_ble_scan(
    adapter_index: usize,
    events: mpsc::Sender<Event>,
    scan_gate: Arc<Mutex<()>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let result = {
                let _scan_guard = scan_gate.lock().await;
                scan_ble(adapter_index, Duration::from_secs(4)).await
            };
            match result {
                Ok(found) => {
                    for device in found.into_iter().filter(|device| {
                        controller_for_ble(device).is_some_and(|controller| {
                            controller.id().as_str().starts_with("nintendo.switch2.")
                        })
                    }) {
                        let _ = events.send(Event::BleDevice(device));
                    }
                }
                Err(error) => {
                    diagnostic(&format!("BLE scan cycle failed: {error}"));
                    let _ = events.send(Event::Log(format!("BLE scan will retry: {error}")));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}
