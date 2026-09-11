use super::*;

impl Handler<Scan> for BluetoothActor {
    type Result = ResponseActFuture<Self, Result<()>>;

    fn handle(&mut self, Scan { request }: Scan, _: &mut Context<Self>) -> Self::Result {
        let transport = match request {
            ScanRequest::Ble { .. } => TransportKind::Ble,
            ScanRequest::BrEdr { .. } => TransportKind::BrEdr,
        };
        if self.connecting_transports.contains(&transport) {
            return Box::pin(fut::ready(Err(Error::Unsupported(
                "discovery is suspended while a controller is connecting",
            ))));
        }
        if !self.active_scans.insert(transport) {
            return Box::pin(fut::ready(Err(Error::Unsupported(
                "a scan is already active on this transport",
            ))));
        }
        let _ = self.events.send(ManagerEvent::ScanState {
            transport,
            active: true,
        });
        let config = self.config.clone();
        let host = self.bredr.clone();
        let gate = self.wake_gate.clone();
        let wii_liac = transport == TransportKind::BrEdr && self.next_wii_liac;
        if transport == TransportKind::BrEdr {
            self.next_wii_liac = !self.next_wii_liac;
        }
        let id = SessionId(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        let (abort, registration) = AbortHandle::new_pair();
        self.scan_tasks
            .insert(transport, PendingOperation { id, abort });
        let scan = async move {
            let mut found = HashMap::new();
            let result =
                Self::scan(&config, host.as_ref(), request, wii_liac, &mut found, &gate).await;
            (found, result)
        };
        Box::pin(Abortable::new(scan, registration).into_actor(self).map(
            move |result, actor, _| {
                let (found, result) =
                    result.map_err(|_| Error::Unsupported("discovery was cancelled"))?;
                actor.scan_finished(id, transport, found, result)
            },
        ))
    }
}

impl Handler<StopScan> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;

    fn handle(&mut self, StopScan { transport }: StopScan, _: &mut Context<Self>) -> Self::Result {
        let suspended = self.suspended_scans.remove(&transport);
        let active = self.active_scans.remove(&transport);
        self.scan_tasks.remove(&transport);
        let adapter = self.config.ble_adapter_index;
        let host = self.bredr.clone();
        let events = self.events.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let result = if active {
                    match transport {
                        TransportKind::Ble => stop_scan(adapter).await,
                        TransportKind::BrEdr => match host {
                            Some(host) => host.cancel_inquiry().await,
                            None => Err(Error::AdapterNotFound),
                        },
                    }
                } else {
                    Ok(())
                };
                if active || suspended {
                    let _ = events.send(ManagerEvent::ScanState {
                        transport,
                        active: false,
                    });
                }
                result
            }
            .into_actor(self),
        ))
    }
}

impl BluetoothActor {
    pub(super) fn scan_finished(
        &mut self,
        operation_id: SessionId,
        transport: TransportKind,
        found: HashMap<DeviceAddr, Discovered>,
        result: Result<()>,
    ) -> Result<()> {
        let Self {
            config,
            events,
            discovered,
            active_scans,
            scan_tasks,
            ..
        } = self;

        if !scan_tasks
            .get(&transport)
            .is_some_and(|task| task.id == operation_id)
        {
            return Err(Error::Unsupported("discovery was cancelled"));
        }
        if !active_scans.remove(&transport) {
            return Err(Error::Unsupported(
                "discovery was suspended for a controller connection",
            ));
        }
        scan_tasks.remove(&transport);
        let mut result = result;
        let mut recognized = Vec::new();
        for (_, device) in found {
            let address = device.identity.address;
            match Self::publish_discovery(
                config,
                device.identity,
                device.bredr,
                device.ble,
                events,
                discovered,
            ) {
                Ok(true) => recognized.push(address),
                Ok(false) => {}
                Err(error) if result.is_ok() => result = Err(error),
                Err(_) => {}
            }
        }
        let _ = events.send(ManagerEvent::ScanState {
            transport,
            active: false,
        });
        for address in recognized {
            if let Some(actor) = self.internal.upgrade() {
                actor.do_send(Connect {
                    address,
                    options: ConnectOptions::default(),
                });
            }
        }

        result
    }
}

impl Handler<WakeDiscovered> for BluetoothActor {
    type Result = ();

    fn handle(
        &mut self,
        WakeDiscovered(record, device, operation_id): WakeDiscovered,
        ctx: &mut Context<Self>,
    ) {
        if !self
            .wake_tasks
            .get(&record.address)
            .is_some_and(|task| task.id() == operation_id)
        {
            return;
        }
        self.wake_tasks.remove(&record.address);
        let identity = Self::ble_identity(&device);
        match Self::publish_discovery(
            &self.config,
            identity,
            None,
            Some(device),
            &self.events,
            &mut self.discovered,
        ) {
            Ok(true) => {
                let options = ConnectOptions {
                    player_index: record.player_index,
                    auto_reconnect: true,
                };
                let connection = self.begin_connect(record.address, options, Some(record));
                ctx.spawn(connection.map(|_, _, _| ()));
            }
            result => {
                let message = result.err().map(|error| error.to_string()).unwrap_or_else(|| "automatic reconnect advertisement does not match a BLE-capable controller profile".to_owned());
                let _ = self.events.send(ManagerEvent::Error {
                    address: Some(record.address),
                    message,
                });
                Self::arm_ble_wake(
                    record,
                    self.config.controllers.clone(),
                    self.internal.clone(),
                    self.wake_gate.clone(),
                    &mut self.wake_tasks,
                );
            }
        }
    }
}

impl BluetoothActor {
    pub(super) fn arm_ble_wake(
        record: ControllerRecord,
        controllers: ControllerRegistry,
        commands: Internal,
        gate: Arc<tokio::sync::Mutex<()>>,
        tasks: &mut HashMap<DeviceAddr, JoinHandle<()>>,
    ) {
        if tasks.contains_key(&record.address) {
            return;
        }
        let address = record.address;
        let adapter = record.adapter_index.unwrap_or(0);
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(2);
            let local_address = default_adapter_address().await.ok();
            loop {
                let found = {
                    let _guard = gate.lock().await;
                    scan_ble(adapter, Duration::from_secs(4))
                        .await
                        .ok()
                        .and_then(|devices| {
                            let controller = controllers.get(&record.profile_id)?;
                            devices.into_iter().find(|device| {
                                device.addr == address
                                    && controller.accepts_reconnect_advertisement(
                                        &Self::ble_identity(device),
                                        local_address,
                                    )
                            })
                        })
                };
                if let Some(device) = found {
                    if let Some(actor) = commands.upgrade() {
                        actor.do_send(WakeDiscovered(record, device, tokio::task::id()));
                    }
                    break;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        tasks.insert(address, task);
    }
}

impl BluetoothActor {
    pub(super) async fn scan(
        config: &ManagerConfig,
        bredr: Option<&Arc<HciHost>>,
        request: ScanRequest,
        wii_liac: bool,
        discovered: &mut HashMap<DeviceAddr, Discovered>,
        wake_gate: &Arc<tokio::sync::Mutex<()>>,
    ) -> Result<()> {
        match request {
            ScanRequest::Ble { duration } => {
                let devices = {
                    let _guard = wake_gate.lock().await;
                    scan_ble(config.ble_adapter_index, duration).await?
                };
                for device in devices {
                    let identity = Self::ble_identity(&device);
                    discovered.insert(
                        identity.address,
                        Discovered {
                            identity,
                            bredr: None,
                            ble: Some(device),
                        },
                    );
                }
                Ok(())
            }
            ScanRequest::BrEdr { duration_units } => {
                let host = bredr.ok_or(Error::AdapterNotFound)?;
                let inquiries = if wii_liac {
                    host.limited_inquiry(duration_units, 0).await?
                } else {
                    host.inquiry(duration_units, 0).await?
                };
                for inquiry in inquiries {
                    let name = host.remote_name(&inquiry).await.ok();
                    let identity = DeviceIdentity {
                        address: inquiry.addr,
                        transport: TransportKind::BrEdr,
                        name,
                        class_of_device: Some(u32::from_le_bytes([
                            inquiry.class_of_device[0],
                            inquiry.class_of_device[1],
                            inquiry.class_of_device[2],
                            0,
                        ])),
                        service_uuids: Vec::new(),
                        manufacturer_data: Vec::new(),
                        pnp: None,
                    };
                    discovered.insert(
                        identity.address,
                        Discovered {
                            identity,
                            bredr: Some(inquiry),
                            ble: None,
                        },
                    );
                }
                Ok(())
            }
        }
    }
}

impl BluetoothActor {
    pub(super) fn ble_identity(device: &BleDevice) -> DeviceIdentity {
        DeviceIdentity {
            address: device.addr,
            transport: TransportKind::Ble,
            name: device.name.clone(),
            class_of_device: None,
            service_uuids: device.services.clone(),
            manufacturer_data: device
                .manufacturer_data
                .iter()
                .map(|(id, bytes)| (*id, bytes.clone()))
                .collect(),
            pnp: None,
        }
    }
}

impl BluetoothActor {
    pub(super) fn publish_discovery(
        config: &ManagerConfig,
        identity: DeviceIdentity,
        bredr: Option<InquiryResult>,
        ble: Option<BleDevice>,
        events: &broadcast::Sender<ManagerEvent>,
        discovered: &mut HashMap<DeviceAddr, Discovered>,
    ) -> Result<bool> {
        let profile_id = config
            .controllers
            .identify(&identity)?
            .map(|controller| controller.id());
        let recognized = profile_id.is_some();
        let rssi = ble
            .as_ref()
            .and_then(|device| device.rssi)
            .or_else(|| bredr.as_ref().and_then(|device| device.rssi).map(i16::from));
        let event = ManagerEvent::Discovered {
            identity: identity.clone(),
            profile_id,
            rssi,
        };
        let replace = recognized
            || discovered.get(&identity.address).is_none_or(|existing| {
                config
                    .controllers
                    .identify(&existing.identity)
                    .ok()
                    .flatten()
                    .is_none()
            });
        if replace {
            discovered.insert(
                identity.address,
                Discovered {
                    identity,
                    bredr,
                    ble,
                },
            );
        }
        let _ = events.send(event);
        Ok(recognized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_scan_completion_cannot_replace_a_new_scan_or_block_cleanup() {
        let config = ManagerConfig::default();
        let (events, mut observed) = broadcast::channel(8);
        let context = Context::<BluetoothActor>::new();
        let mut actor = BluetoothActor::new(config, events, context.address().downgrade());
        let (abort, _) = AbortHandle::new_pair();
        actor.active_scans.insert(TransportKind::BrEdr);
        actor.scan_tasks.insert(
            TransportKind::BrEdr,
            PendingOperation {
                id: SessionId(2),
                abort,
            },
        );
        let address = DeviceAddr::public([1, 2, 3, 4, 5, 6]);
        let found = HashMap::from([(
            address,
            Discovered {
                identity: wiimote_identity(address, TransportKind::BrEdr),
                bredr: None,
                ble: None,
            },
        )]);
        assert!(
            actor
                .scan_finished(SessionId(1), TransportKind::BrEdr, found, Ok(()))
                .is_err()
        );
        assert!(actor.discovered.is_empty());
        assert_eq!(actor.scan_tasks[&TransportKind::BrEdr].id, SessionId(2));
        assert!(observed.try_recv().is_err());
        time::timeout(Duration::from_secs(1), actor.cleanup())
            .await
            .unwrap();
        assert!(actor.scan_tasks.is_empty());
        assert!(actor.active_scans.is_empty());
    }

    fn wiimote_identity(address: DeviceAddr, transport: TransportKind) -> DeviceIdentity {
        DeviceIdentity {
            address,
            transport,
            name: Some("Nintendo RVL-CNT-01".to_owned()),
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        }
    }

    #[test]
    fn unrecognized_ble_observation_does_not_replace_classic_controller() {
        let config = ManagerConfig::default();
        let (events, _) = broadcast::channel(8);
        let address = DeviceAddr::public([1, 2, 3, 4, 5, 6]);
        let mut discovered = HashMap::new();

        assert!(
            BluetoothActor::publish_discovery(
                &config,
                wiimote_identity(address, TransportKind::BrEdr),
                None,
                None,
                &events,
                &mut discovered,
            )
            .unwrap()
        );
        assert!(
            !BluetoothActor::publish_discovery(
                &config,
                wiimote_identity(address, TransportKind::Ble),
                None,
                None,
                &events,
                &mut discovered,
            )
            .unwrap()
        );
        assert_eq!(
            discovered.get(&address).unwrap().identity.transport,
            TransportKind::BrEdr
        );
    }
}
