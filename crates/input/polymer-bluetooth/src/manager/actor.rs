use super::*;

impl BluetoothActor {
    pub(super) async fn cleanup(&mut self) {
        for (_, task) in self.scan_tasks.drain() {
            task.cancel().await;
        }
        for (_, task) in self.wake_tasks.drain() {
            task.abort();
            let _ = task.await;
        }
        for transport in self.active_scans.drain() {
            match transport {
                TransportKind::Ble => {
                    let _ = stop_scan(self.config.ble_adapter_index).await;
                }
                TransportKind::BrEdr => {
                    if let Some(host) = &self.bredr {
                        let _ = host.cancel_inquiry().await;
                    }
                }
            }
            let _ = self.events.send(ManagerEvent::ScanState {
                transport,
                active: false,
            });
        }
        self.connecting_transports.clear();
        self.suspended_scans.clear();
        for (_, task) in self.connect_tasks.drain() {
            task.cancel().await;
        }
        for (_, mut session) in self.sessions.drain() {
            let slept = match session.controller.sleep(session.transport.as_mut()).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::debug!(
                        %error,
                        session_id = session.info.session_id.get(),
                        "controller does not support sleeping during shutdown; disconnecting"
                    );
                    false
                }
            };
            session.report_task.abort();
            if let Err(error) = session.transport.disconnect().await {
                tracing::warn!(
                    %error,
                    session_id = session.info.session_id.get(),
                    "failed to disconnect controller during shutdown"
                );
            }
            let _ = self.events.send(ManagerEvent::Disconnected {
                controller_id: session.info.controller_id,
                session_id: session.info.session_id,
                reason: if slept {
                    "sleep requested"
                } else {
                    "disconnected"
                }
                .into(),
                reconnecting: false,
            });
        }
        if let Some(task) = self.event_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.pairing_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.pairing.take();
        self.bredr.take();
    }
}

impl BluetoothActor {
    pub(super) async fn initialize_adapter<T, F, Future>(
        label: &'static str,
        mut initialize: F,
    ) -> Result<T>
    where
        F: FnMut() -> Future,
        Future: std::future::Future<Output = Result<T>>,
    {
        for attempt in 1..=ADAPTER_INITIALIZATION_ATTEMPTS {
            let result = time::timeout(ADAPTER_INITIALIZATION_TIMEOUT, initialize()).await;
            match result {
                Ok(Ok(adapter)) => return Ok(adapter),
                Ok(Err(error)) if attempt == ADAPTER_INITIALIZATION_ATTEMPTS => return Err(error),
                Err(_) if attempt == ADAPTER_INITIALIZATION_ATTEMPTS => {
                    return Err(Error::Timeout(label));
                }
                Ok(Err(error)) => {
                    tracing::warn!(attempt, %error, label, "retrying Bluetooth adapter initialization");
                }
                Err(_) => {
                    tracing::warn!(
                        attempt,
                        label,
                        "Bluetooth adapter initialization timed out; retrying"
                    );
                }
            }
            time::sleep(ADAPTER_INITIALIZATION_RETRY_DELAY).await;
        }
        unreachable!("adapter initialization attempts are nonzero")
    }
}

impl Actor for BluetoothActor {
    type Args = (ManagerConfig, broadcast::Sender<ManagerEvent>);
    type Error = Error;

    async fn on_start((config, events): Self::Args, actor: ActorRef<Self>) -> Result<Self> {
        let internal = actor.downgrade();
        let (bredr, ble_names) = tokio::join!(
            Self::initialize_adapter("BR/EDR adapter", || {
                HciHost::open_with_firmware(&config.bredr_adapter, config.firmware.clone())
            }),
            Self::initialize_adapter("BLE adapter", BleGatt::adapter_names),
        );
        let bredr = match bredr {
            Ok(host) => {
                let host = Arc::new(host);
                let info = host.info();
                if let Some(source) = &info.firmware_source {
                    let _ = events.send(ManagerEvent::FirmwareLoaded {
                        family: source.clone(),
                    });
                }
                let _ = events.send(ManagerEvent::AdapterReady {
                    transport: TransportKind::BrEdr,
                    name: format!(
                        "{:04x}:{:04x} ({})",
                        info.usb_vendor_id, info.usb_product_id, info.address
                    ),
                });
                Some(host)
            }
            Err(error) => {
                let _ = events.send(ManagerEvent::AdapterUnavailable {
                    transport: TransportKind::BrEdr,
                    error: error.to_string(),
                });
                None
            }
        };
        match ble_names {
            Ok(names) if config.ble_adapter_index < names.len() => {
                let _ = events.send(ManagerEvent::AdapterReady {
                    transport: TransportKind::Ble,
                    name: names[config.ble_adapter_index].clone(),
                });
            }
            Ok(_) => {
                let _ = events.send(ManagerEvent::AdapterUnavailable {
                    transport: TransportKind::Ble,
                    error: "selected OS BLE adapter does not exist".into(),
                });
            }
            Err(error) => {
                let _ = events.send(ManagerEvent::AdapterUnavailable {
                    transport: TransportKind::Ble,
                    error: error.to_string(),
                });
            }
        }

        let keys = Arc::new(ControllerLinkKeyStore::new(config.store.clone()));
        let pairing = bredr
            .as_ref()
            .map(|host| Arc::new(PairingAgent::new(host.clone(), keys.clone())));
        let pairing_task = pairing
            .as_ref()
            .map(|agent| tokio::spawn(agent.clone().run()));
        let discovered = HashMap::<DeviceAddr, Discovered>::new();
        let active_scans = HashSet::<TransportKind>::new();
        let scan_tasks = HashMap::<TransportKind, PendingOperation>::new();
        let connect_tasks = HashMap::<DeviceAddr, PendingOperation>::new();
        let connecting_transports = HashSet::<TransportKind>::new();
        let suspended_scans = HashSet::<TransportKind>::new();
        let next_wii_liac = true;
        let sessions = HashMap::<DeviceAddr, Session>::new();
        let next_session_id = 1_u64;

        let wake_gate = Arc::new(tokio::sync::Mutex::new(()));
        let mut wake_tasks = HashMap::<DeviceAddr, JoinHandle<()>>::new();
        for record in config.store.records().unwrap_or_default() {
            let controller = config.controllers.get(&record.profile_id);
            if controller.is_none() {
                let _ = events.send(ManagerEvent::Error {
                    address: Some(record.address),
                    message: Error::MissingProfile(record.profile_id.to_string()).to_string(),
                });
            } else if record.transport == TransportKind::Ble
                && record.auto_reconnect
                && controller
                    .is_some_and(|controller| controller.supports_transport(TransportKind::Ble))
            {
                Self::arm_ble_wake(
                    record,
                    config.controllers.clone(),
                    internal.clone(),
                    wake_gate.clone(),
                    &mut wake_tasks,
                );
            }
        }

        let event_task = bredr.as_ref().map(|host| {
            let mut receiver = host.events();
            let internal = internal.clone();
            tokio::spawn(async move {
                loop {
                    let event = receiver.recv().await;
                    let closed = matches!(event, Err(broadcast::error::RecvError::Closed));
                    if !matches!(event, Ok(HciEvent::ConnectionRequest { .. }) | Err(_)) {
                        continue;
                    }
                    let Some(actor) = internal.upgrade() else {
                        break;
                    };
                    if actor.tell(Command::HciEvent(event)).send().await.is_err() || closed {
                        break;
                    }
                }
            })
        });
        Ok(Self {
            config,
            bredr,
            pairing,
            keys,
            internal,
            events,
            discovered,
            active_scans,
            scan_tasks,
            connect_tasks,
            connecting_transports,
            suspended_scans,
            next_wii_liac,
            sessions,
            next_session_id,
            wake_gate,
            wake_tasks,
            pairing_task,
            event_task,
        })
    }

    async fn on_stop(
        &mut self,
        _: WeakActorRef<Self>,
        _: kameo::error::ActorStopReason,
    ) -> Result<()> {
        self.cleanup().await;
        Ok(())
    }
}

impl Message<Command> for BluetoothActor {
    type Reply = ();
    async fn handle(&mut self, command: Command, _: &mut Context<Self, ()>) {
        match command {
            Command::HciEvent(event) => self.hci_event(event).await,
            Command::ScanFinished {
                operation_id,
                transport,
                discovered: found,
                result,
                response,
            } => response.send(
                self.scan_finished(operation_id, transport, found, result)
                    .await,
            ),
            Command::ConnectFinished {
                session_id,
                address,
                transport,
                result,
                response,
                reconnect,
            } => {
                self.connect_finished(session_id, address, transport, result, response, reconnect)
                    .await
            }
            Command::WakeDiscovered(record, device, operation_id) => {
                self.wake_discovered(record, device, operation_id).await
            }
            Command::SessionEnded {
                controller_id,
                session_id,
                reason,
            } => self.session_ended(controller_id, session_id, reason).await,
        }
    }
}
