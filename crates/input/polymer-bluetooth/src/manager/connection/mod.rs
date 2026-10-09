mod transport;

use super::*;

impl Handler<Connect> for BluetoothActor {
    type Result = ResponseActFuture<Self, Result<()>>;
    fn handle(
        &mut self,
        Connect { address, options }: Connect,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.begin_connect(address, options, None)
    }
}

impl BluetoothActor {
    pub(super) fn begin_connect(
        &mut self,
        address: DeviceAddr,
        options: ConnectOptions,
        reconnect: Option<ControllerRecord>,
    ) -> ResponseActFuture<Self, Result<()>> {
        if self.sessions.contains_key(&address) {
            return Box::pin(fut::ready(Ok(())));
        }
        let device = self.discovered.get(&address).cloned();
        let error = if self.connect_tasks.contains_key(&address) {
            Some("a connection attempt is already active for this controller")
        } else if device.is_none() {
            Some("device must be discovered before connecting")
        } else if self
            .connecting_transports
            .contains(&device.as_ref().unwrap().identity.transport)
        {
            Some("another controller connection is active on this transport")
        } else {
            None
        };
        if let Some(error) = error {
            if let Some(record) = reconnect {
                Self::arm_ble_wake(
                    record,
                    self.config.controllers.clone(),
                    self.internal.clone(),
                    self.wake_gate.clone(),
                    &mut self.wake_tasks,
                );
            }
            return Box::pin(fut::ready(Err(Error::Unsupported(error))));
        }
        let device = device.expect("discovered device checked above");
        let transport = device.identity.transport;
        self.connecting_transports.insert(transport);
        let suspend = self.active_scans.remove(&transport);
        self.scan_tasks.remove(&transport);
        if suspend {
            self.suspended_scans.insert(transport);
        }
        let id = SessionId(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        let context = ConnectContext {
            config: self.config.clone(),
            host: self.bredr.clone(),
            pairing: self.pairing.clone(),
            keys: self.keys.clone(),
            events: self.events.clone(),
        };
        let (abort, registration) = AbortHandle::new_pair();
        self.connect_tasks
            .insert(address, PendingOperation { id, abort });
        let connect = async move {
            if suspend {
                match transport {
                    TransportKind::Ble => {
                        let _ = stop_scan(context.config.ble_adapter_index).await;
                    }
                    TransportKind::BrEdr => {
                        if let Some(host) = &context.host {
                            let _ = host.cancel_inquiry().await;
                        }
                    }
                }
            }
            Self::connect(context, address, id, options, device).await
        };
        Box::pin(
            Abortable::new(
                time::timeout(CONNECTION_ATTEMPT_TIMEOUT, connect),
                registration,
            )
            .into_actor(self)
            .then(move |result, actor, _| {
                let result = result
                    .map_err(|_| Error::Unsupported("connection was cancelled"))
                    .and_then(|result| {
                        result.unwrap_or(Err(Error::Timeout("controller connection")))
                    });
                actor.finish_connection(id, address, transport, result, reconnect)
            }),
        )
    }

    fn finish_connection(
        &mut self,
        id: SessionId,
        address: DeviceAddr,
        transport: TransportKind,
        result: Result<Session>,
        reconnect: Option<ControllerRecord>,
    ) -> ResponseActFuture<Self, Result<()>> {
        if !self
            .connect_tasks
            .get(&address)
            .is_some_and(|task| task.id == id)
        {
            return Box::pin(
                async move {
                    if let Ok(mut session) = result {
                        let _ = session.transport.disconnect().await;
                    }
                    Err(Error::Unsupported("connection was cancelled"))
                }
                .into_actor(self),
            );
        }
        self.connect_tasks.remove(&address);
        self.connecting_transports.remove(&transport);
        let result = result.and_then(|session| {
            let disconnections = session.transport.disconnections();
            if session.transport.state() == crate::ConnectionState::Disconnected {
                return Err(Error::Unsupported(
                    "controller disconnected before its session started",
                ));
            }
            Ok((session, disconnections))
        });
        let result = match result {
            Ok((mut session, disconnections)) => {
                session.report_task = Some(Self::spawn_reports(
                    session.info.controller_id,
                    id,
                    session.transport.reports(),
                    disconnections,
                    self.events.clone(),
                    self.internal.clone(),
                ));
                let _ = self
                    .events
                    .send(ManagerEvent::Connected(session.info.clone()));
                self.sessions.insert(address, session);
                Ok(())
            }
            Err(error) => {
                let _ = self.events.send(ManagerEvent::Error {
                    address: Some(address),
                    message: error.to_string(),
                });
                if let Some(record) = reconnect {
                    Self::arm_ble_wake(
                        record,
                        self.config.controllers.clone(),
                        self.internal.clone(),
                        self.wake_gate.clone(),
                        &mut self.wake_tasks,
                    );
                }
                Err(error)
            }
        };
        if self.suspended_scans.remove(&transport) {
            let _ = self.events.send(ManagerEvent::ScanState {
                transport,
                active: false,
            });
        }
        Box::pin(fut::ready(result))
    }
}

impl Handler<Forget> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(&mut self, Forget { controller_id }: Forget, _: &mut Context<Self>) -> Self::Result {
        if self.connect_tasks.remove(&controller_id.address).is_some() {
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
        let session = if self
            .sessions
            .get(&controller_id.address)
            .is_some_and(|session| session.info.controller_id == controller_id)
        {
            self.sessions.remove(&controller_id.address)
        } else {
            None
        };
        let events = self.events.clone();
        let pairing = self.pairing.clone();
        let store = self.config.store.clone();
        AtomicResponse::new(Box::pin(
            async move {
                if let Some(mut session) = session {
                    session.stop_reports();
                    let _ = session.transport.disconnect().await;
                    let _ = events.send(ManagerEvent::Disconnected {
                        controller_id,
                        session_id: session.info.session_id,
                        reason: "disconnected".into(),
                        reconnecting: false,
                    });
                }
                if let Some(pairing) = pairing {
                    let _ = pairing.forget(controller_id.address);
                }
                store.remove(controller_id.address)
            }
            .into_actor(self),
        ))
    }
}

impl Handler<HciLifecycle> for BluetoothActor {
    type Result = ResponseActFuture<Self, ()>;
    fn handle(&mut self, HciLifecycle(event): HciLifecycle, _: &mut Context<Self>) -> Self::Result {
        let addr = match event {
            Ok(HciEvent::ConnectionRequest { addr, .. }) => addr,
            Err(broadcast::error::RecvError::Lagged(count)) => {
                let _ = self.events.send(ManagerEvent::Error {
                    address: None,
                    message: format!("dropped {count} HCI lifecycle events"),
                });
                return Box::pin(fut::ready(()));
            }
            _ => return Box::pin(fut::ready(())),
        };
        let Some(host) = self.bredr.clone() else {
            return Box::pin(fut::ready(()));
        };
        if self.sessions.contains_key(&addr)
            || self.connect_tasks.contains_key(&addr)
            || !self.connecting_transports.insert(TransportKind::BrEdr)
        {
            return Box::pin(
                async move {
                    let _ = host.reject_connection(addr).await;
                }
                .into_actor(self),
            );
        }
        let id = SessionId(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        let config = self.config.clone();
        let keys = self.keys.clone();
        let discovered = self.discovered.get(&addr).cloned();
        let (abort, registration) = AbortHandle::new_pair();
        self.connect_tasks
            .insert(addr, PendingOperation { id, abort });
        let accept = async move {
            let result = time::timeout(
                CONNECTION_ATTEMPT_TIMEOUT,
                Self::accept_inbound(&config, &host, keys, addr, id, discovered.as_ref()),
            )
            .await
            .unwrap_or(Err(Error::Timeout("inbound controller connection")));
            if result.is_err() {
                let _ = host.reject_connection(addr).await;
            }
            result
        };
        Box::pin(
            Abortable::new(accept, registration)
                .into_actor(self)
                .then(move |result, actor, _| {
                    actor.finish_connection(
                        id,
                        addr,
                        TransportKind::BrEdr,
                        result.unwrap_or(Err(Error::Unsupported("connection was cancelled"))),
                        None,
                    )
                })
                .map(|_, _, _| ()),
        )
    }
}
