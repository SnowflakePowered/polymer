mod transport;

use super::*;

impl Message<Connect> for BluetoothActor {
    type Reply = DelegatedReply<Result<()>>;

    async fn handle(
        &mut self,
        Connect { address, options }: Connect,
        context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let (delegated, response) = context.reply_sender();
        let Some(response) = response else {
            return delegated;
        };

        if self.sessions.contains_key(&address) {
            response.send(Ok(()));
            return delegated;
        }
        if self.connect_tasks.contains_key(&address) {
            response.send(Err(Error::Unsupported(
                "a connection attempt is already active for this controller",
            )));
            return delegated;
        }
        let Some(device) = self.discovered.get(&address).cloned() else {
            response.send(Err(Error::Unsupported(
                "device must be discovered before connecting",
            )));
            return delegated;
        };
        let transport = device.identity.transport;
        if !self.connecting_transports.insert(transport) {
            response.send(Err(Error::Unsupported(
                "another controller connection is active on this transport",
            )));
            return delegated;
        }
        if self.active_scans.remove(&transport) {
            if let Some(task) = self.scan_tasks.remove(&transport) {
                task.cancel().await;
            }
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
            self.suspended_scans.insert(transport);
        }
        let session_id = SessionId(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        let task = BluetoothActor::spawn_connect(
            ConnectContext {
                config: self.config.clone(),
                host: self.bredr.clone(),
                pairing: self.pairing.clone(),
                keys: self.keys.clone(),
                internal: self.internal.clone(),
                events: self.events.clone(),
            },
            address,
            session_id,
            options,
            device,
            Some(response),
            None,
        );
        self.connect_tasks.insert(address, task);
        delegated
    }
}

impl Message<Forget> for BluetoothActor {
    type Reply = Result<()>;

    async fn handle(
        &mut self,
        Forget { controller_id }: Forget,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(operation) = self.connect_tasks.remove(&controller_id.address) {
            operation.cancel().await;
            self.connecting_transports.remove(&controller_id.transport);
            if self.suspended_scans.remove(&controller_id.transport) {
                let _ = self.events.send(ManagerEvent::ScanState {
                    transport: controller_id.transport,
                    active: false,
                });
            }
        }
        if let Some(task) = self.wake_tasks.remove(&controller_id.address) {
            task.abort();
        }
        let active_session_id = self
            .sessions
            .values()
            .find(|session| session.info.controller_id == controller_id)
            .map(|session| session.info.session_id);
        if let Some(session_id) = active_session_id {
            let _ = BluetoothActor::disconnect(session_id, false, &self.events, &mut self.sessions)
                .await;
        }
        if let Some(pairing) = &self.pairing {
            let _ = pairing.forget(controller_id.address);
        }
        self.config.store.remove(controller_id.address)
    }
}

impl BluetoothActor {
    pub(super) async fn hci_event(
        &mut self,
        event: std::result::Result<HciEvent, broadcast::error::RecvError>,
    ) {
        match event {
            Ok(HciEvent::ConnectionRequest { addr, .. }) => {
                let Some(host) = self.bredr.clone() else {
                    return;
                };
                if self.sessions.contains_key(&addr)
                    || self.connect_tasks.contains_key(&addr)
                    || !self.connecting_transports.insert(TransportKind::BrEdr)
                {
                    let _ = host.reject_connection(addr).await;
                    return;
                }
                let session_id = SessionId(self.next_session_id);
                self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
                let config = self.config.clone();
                let events = self.events.clone();
                let internal = self.internal.clone();
                let discovered = self.discovered.get(&addr).cloned();
                let task = tokio::spawn(async move {
                    let result = time::timeout(
                        CONNECTION_ATTEMPT_TIMEOUT,
                        Self::accept_inbound(
                            &config,
                            &host,
                            addr,
                            session_id,
                            &events,
                            internal.clone(),
                            discovered.as_ref(),
                        ),
                    )
                    .await
                    .unwrap_or(Err(Error::Timeout("inbound controller connection")));
                    if result.is_err() {
                        let _ = host.reject_connection(addr).await;
                    }
                    if let Some(actor) = internal.upgrade() {
                        let _ = actor
                            .tell(Command::ConnectFinished {
                                session_id,
                                address: addr,
                                transport: TransportKind::BrEdr,
                                result,
                                response: None,
                                reconnect: None,
                            })
                            .send()
                            .await;
                    }
                });
                self.connect_tasks.insert(
                    addr,
                    PendingOperation {
                        id: session_id,
                        task,
                    },
                );
            }
            Err(broadcast::error::RecvError::Lagged(count)) => {
                let _ = self.events.send(ManagerEvent::Error {
                    address: None,
                    message: format!("dropped {count} HCI lifecycle events"),
                });
            }
            _ => {}
        }
    }
}

impl BluetoothActor {
    pub(super) async fn auto_connect(&mut self, address: DeviceAddr, options: ConnectOptions) {
        let Self {
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
            sessions,
            next_session_id,
            ..
        } = self;

        if sessions.contains_key(&address) {
            return;
        }
        if connect_tasks.contains_key(&address) {
            return;
        }
        let Some(device) = discovered.get(&address).cloned() else {
            return;
        };
        let transport = device.identity.transport;
        if !connecting_transports.insert(transport) {
            return;
        }
        if active_scans.remove(&transport) {
            if let Some(task) = scan_tasks.remove(&transport) {
                task.cancel().await;
            }
            match transport {
                TransportKind::Ble => {
                    let _ = stop_scan(config.ble_adapter_index).await;
                }
                TransportKind::BrEdr => {
                    if let Some(host) = &bredr {
                        let _ = host.cancel_inquiry().await;
                    }
                }
            }
            suspended_scans.insert(transport);
        }
        let session_id = SessionId(*next_session_id);
        *next_session_id = next_session_id.wrapping_add(1).max(1);
        let task = Self::spawn_connect(
            ConnectContext {
                config: config.clone(),
                host: bredr.clone(),
                pairing: pairing.clone(),
                keys: keys.clone(),
                internal: internal.clone(),
                events: events.clone(),
            },
            address,
            session_id,
            options,
            device,
            None,
            None,
        );
        connect_tasks.insert(address, task);
    }
}

impl BluetoothActor {
    pub(super) async fn connect_finished(
        &mut self,
        session_id: SessionId,
        address: DeviceAddr,
        transport: TransportKind,
        result: Result<Session>,
        response: Option<ReplySender<Result<()>>>,
        reconnect: Option<ControllerRecord>,
    ) {
        let Self {
            config,
            internal,
            events,
            connect_tasks,
            connecting_transports,
            suspended_scans,
            sessions,
            wake_gate,
            wake_tasks,
            ..
        } = self;

        if !connect_tasks
            .get(&address)
            .is_some_and(|task| task.id == session_id)
        {
            if let Ok(mut session) = result {
                let _ = session.transport.disconnect().await;
            }
            if let Some(response) = response {
                response.send(Err(Error::Unsupported("connection was cancelled")));
            }
            return;
        }
        connect_tasks.remove(&address);
        connecting_transports.remove(&transport);
        let result = match result {
            Ok(session) => {
                let _ = events.send(ManagerEvent::Connected(session.info.clone()));
                sessions.insert(address, session);
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                let _ = events.send(ManagerEvent::Error {
                    address: Some(address),
                    message: message.clone(),
                });
                if let Some(record) = reconnect {
                    Self::arm_ble_wake(
                        record,
                        config.controllers.clone(),
                        internal.clone(),
                        wake_gate.clone(),
                        wake_tasks,
                    );
                }
                Err(error)
            }
        };
        if let Some(response) = response {
            response.send(result);
        }
        if suspended_scans.remove(&transport) {
            let _ = events.send(ManagerEvent::ScanState {
                transport,
                active: false,
            });
        }
    }
}
