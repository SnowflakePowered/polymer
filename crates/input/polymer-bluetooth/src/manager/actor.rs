use super::*;

impl BluetoothActor {
    pub(super) fn new(
        config: ManagerConfig,
        events: broadcast::Sender<ManagerEvent>,
        internal: Internal,
    ) -> Self {
        Self {
            keys: Arc::new(ControllerLinkKeyStore::new(config.store.clone())),
            config,
            events,
            internal,
            bredr: None,
            pairing: None,
            discovered: HashMap::new(),
            active_scans: HashSet::new(),
            scan_tasks: HashMap::new(),
            connect_tasks: HashMap::new(),
            connecting_transports: HashSet::new(),
            suspended_scans: HashSet::new(),
            next_wii_liac: true,
            sessions: HashMap::new(),
            next_session_id: 1,
            wake_gate: Arc::new(tokio::sync::Mutex::new(())),
            wake_tasks: HashMap::new(),
            pairing_task: None,
            event_task: None,
            shutdown_complete: false,
        }
    }

    /// Move hardware resources into the cleanup future; no actor state is borrowed across await.
    pub(super) fn cleanup(&mut self) -> impl std::future::Future<Output = ()> + use<> {
        self.scan_tasks.clear();
        self.connect_tasks.clear();
        self.connecting_transports.clear();
        self.suspended_scans.clear();
        let wake_tasks = std::mem::take(&mut self.wake_tasks);
        for task in wake_tasks.values() {
            task.abort();
        }
        let active_scans = std::mem::take(&mut self.active_scans);
        let sessions = std::mem::take(&mut self.sessions);
        let host = self.bredr.take();
        let pairing = self.pairing.take();
        let event_task = self.event_task.take();
        let pairing_task = self.pairing_task.take();
        let events = self.events.clone();
        let adapter = self.config.ble_adapter_index;
        async move {
            for (_, task) in wake_tasks {
                let _ = task.await;
            }
            for transport in active_scans {
                match transport {
                    TransportKind::Ble => {
                        let _ = stop_scan(adapter).await;
                    }
                    TransportKind::BrEdr => {
                        if let Some(host) = &host {
                            let _ = host.cancel_inquiry().await;
                        }
                    }
                }
                let _ = events.send(ManagerEvent::ScanState {
                    transport,
                    active: false,
                });
            }
            for (_, mut session) in sessions {
                let slept = match session.controller.sleep(session.transport.as_mut()).await {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::debug!(%error, session_id = session.info.session_id.get(), "controller does not support sleeping during shutdown; disconnecting");
                        false
                    }
                };
                session.stop_reports();
                if let Err(error) = session.transport.disconnect().await {
                    tracing::warn!(%error, session_id = session.info.session_id.get(), "failed to disconnect controller during shutdown");
                }
                let _ = events.send(ManagerEvent::Disconnected {
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
            if let Some(task) = event_task {
                task.abort();
                let _ = task.await;
            }
            if let Some(task) = pairing_task {
                task.abort();
                let _ = task.await;
            }
            drop(pairing);
            drop(host);
        }
    }
}

impl Actor for BluetoothActor {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        Self::initialize(
            self.config.clone(),
            self.events.clone(),
            self.internal.clone(),
        )
        .into_actor(self)
        .map(|initialized, actor, _| *actor = initialized)
        .wait(ctx);
    }

    fn stopping(&mut self, ctx: &mut Self::Context) -> Running {
        if self.shutdown_complete {
            return Running::Stop;
        }
        self.cleanup()
            .into_actor(self)
            .map(|(), actor, ctx| {
                actor.shutdown_complete = true;
                ctx.stop();
            })
            .wait(ctx);
        Running::Continue
    }

    fn stopped(&mut self, _: &mut Self::Context) {
        System::current().stop();
    }
}

impl Handler<Shutdown> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;

    fn handle(&mut self, _: Shutdown, _: &mut Context<Self>) -> Self::Result {
        AtomicResponse::new(Box::pin(self.cleanup().into_actor(self).map(
            |(), actor, ctx| {
                actor.shutdown_complete = true;
                ctx.stop();
                Ok(())
            },
        )))
    }
}
