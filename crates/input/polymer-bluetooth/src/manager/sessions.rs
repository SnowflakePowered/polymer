use super::*;

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn send_raw(&mut self, session_id: SessionId, bytes: Vec<u8>) -> Result<()> {
        match BluetoothActor::session_mut(&mut self.sessions, session_id) {
            Some(session) => {
                session
                    .controller
                    .send_raw_hid(session.transport.as_mut(), &bytes)
                    .await
            }
            None => Err(Error::Unsupported("controller is not connected")),
        }
    }
}

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn set_external_ownership(&mut self, session_id: SessionId, enabled: bool) -> Result<()> {
        match BluetoothActor::session_mut(&mut self.sessions, session_id) {
            Some(session) => {
                let result = session
                    .controller
                    .set_external_ownership(session.transport.as_mut(), enabled)
                    .await;
                if result.is_ok() {
                    session.externally_owned = enabled;
                }
                result
            }
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }
}

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn set_player_index(&mut self, session_id: SessionId, index: u8) -> Result<()> {
        match BluetoothActor::session_mut(&mut self.sessions, session_id) {
            Some(session) if !session.externally_owned => {
                let result = session
                    .controller
                    .set_player_led(session.transport.as_mut(), index)
                    .await;
                if result.is_ok() {
                    session.info.player_index = index;
                    if let Err(error) = BluetoothActor::persist_session(&self.config, session) {
                        let _ = self.events.send(ManagerEvent::Error {
                            address: Some(session.info.controller_id.address),
                            message: format!("failed to persist player index: {error}"),
                        });
                    }
                }
                result
            }
            Some(_) => Ok(()),
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }
}

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn set_rumble(
        &mut self,
        session_id: SessionId,
        low_frequency: u16,
        high_frequency: u16,
    ) -> Result<()> {
        match BluetoothActor::session_mut(&mut self.sessions, session_id) {
            Some(session) if !session.externally_owned => {
                session
                    .controller
                    .set_rumble(session.transport.as_mut(), low_frequency, high_frequency)
                    .await
            }
            Some(_) => Ok(()),
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }
}

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn set_ir_camera(
        &mut self,
        session_id: SessionId,
        setup: crate::devices::IrCameraSetup,
    ) -> Result<()> {
        match BluetoothActor::session_mut(&mut self.sessions, session_id) {
            Some(session) if !session.externally_owned => {
                session
                    .controller
                    .set_ir_camera(session.transport.as_mut(), setup)
                    .await
            }
            Some(_) => Ok(()),
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }
}

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn sleep(&mut self, session_id: SessionId) -> Result<()> {
        self.disconnect_session(session_id, true).await
    }
}

#[native_handler(BluetoothManager)]
impl BluetoothActor {
    async fn disconnect(&mut self, session_id: SessionId) -> Result<()> {
        self.disconnect_session(session_id, false).await
    }
}

impl BluetoothActor {
    pub(super) async fn disconnect_session(
        &mut self,
        session_id: SessionId,
        sleep: bool,
    ) -> Result<()> {
        let controller_id = BluetoothActor::session(&self.sessions, session_id)
            .map(|session| session.info.controller_id);
        let result =
            BluetoothActor::disconnect(session_id, sleep, &self.events, &mut self.sessions).await;
        if sleep
            && result.is_ok()
            && let Some(controller_id) = controller_id
            && let Some(record) = self
                .config
                .store
                .records()
                .unwrap_or_default()
                .into_iter()
                .find(|record| {
                    record.address == controller_id.address
                        && record.transport == TransportKind::Ble
                        && record.auto_reconnect
                })
        {
            BluetoothActor::arm_ble_wake(
                record,
                self.config.controllers.clone(),
                self.internal.clone(),
                self.wake_gate.clone(),
                &mut self.wake_tasks,
            );
        }
        result
    }
}

impl BluetoothActor {
    pub(super) async fn session_ended(
        &mut self,
        controller_id: ControllerId,
        session_id: SessionId,
        reason: String,
    ) {
        let Self {
            config,
            internal,
            events,
            sessions,
            wake_gate,
            wake_tasks,
            ..
        } = self;

        let is_current = sessions
            .get(&controller_id.address)
            .is_some_and(|session| session.info.session_id == session_id);
        if !is_current {
            return;
        }
        let session = sessions
            .remove(&controller_id.address)
            .expect("session checked above");
        let wake_record = (session.info.identity.transport == TransportKind::Ble)
            .then(|| {
                config
                    .store
                    .records()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|record| record.address == controller_id.address && record.auto_reconnect)
            })
            .flatten();
        let _ = events.send(ManagerEvent::Disconnected {
            controller_id,
            session_id,
            reason,
            reconnecting: wake_record.is_some(),
        });
        if let Some(record) = wake_record {
            Self::arm_ble_wake(
                record,
                config.controllers.clone(),
                internal.clone(),
                wake_gate.clone(),
                wake_tasks,
            );
        }
    }
}

impl BluetoothActor {
    pub(super) fn spawn_reports(
        controller_id: ControllerId,
        session_id: SessionId,
        mut reports: crate::ReportReceiver,
        mut disconnections: Option<crate::DisconnectReceiver>,
        events: broadcast::Sender<ManagerEvent>,
        internal: Internal,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let reason = loop {
                tokio::select! {
                    report = reports.recv() => {
                        match report {
                            Ok(bytes) => {
                                let _ = events.send(ManagerEvent::RawReport {
                                    controller_id,
                                    session_id,
                                    received_at: std::time::Instant::now(),
                                    bytes,
                                });
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => {
                                break "input report stream closed".to_owned();
                            }
                        }
                    }
                    disconnected = Self::receive_disconnect(&mut disconnections) => {
                        if disconnected {
                            break "physical controller disconnected".to_owned();
                        }
                    }
                }
            };
            let _ = internal
                .send(Command::SessionEnded {
                    controller_id,
                    session_id,
                    reason,
                })
                .await;
        })
    }
}

impl BluetoothActor {
    pub(super) async fn receive_disconnect(
        receiver: &mut Option<crate::DisconnectReceiver>,
    ) -> bool {
        match receiver {
            Some(receiver) => loop {
                match receiver.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Closed) => return true,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            },
            None => std::future::pending().await,
        }
    }
}

impl BluetoothActor {
    pub(super) fn session(
        sessions: &HashMap<DeviceAddr, Session>,
        session_id: SessionId,
    ) -> Option<&Session> {
        sessions
            .values()
            .find(|session| session.info.session_id == session_id)
    }
}

impl BluetoothActor {
    pub(super) fn session_mut(
        sessions: &mut HashMap<DeviceAddr, Session>,
        session_id: SessionId,
    ) -> Option<&mut Session> {
        sessions
            .values_mut()
            .find(|session| session.info.session_id == session_id)
    }
}

impl BluetoothActor {
    pub(super) fn persist_session(config: &ManagerConfig, session: &Session) -> Result<()> {
        let Some(mut record) = config
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == session.info.controller_id.address)
        else {
            return Err(Error::Unsupported(
                "connected controller has no persisted record",
            ));
        };
        record.player_index = session.info.player_index;
        config.store.put(record)
    }
}

impl BluetoothActor {
    pub(super) async fn disconnect(
        session_id: SessionId,
        sleep: bool,
        events: &broadcast::Sender<ManagerEvent>,
        sessions: &mut HashMap<DeviceAddr, Session>,
    ) -> Result<()> {
        let Some(address) = sessions
            .iter()
            .find(|(_, session)| session.info.session_id == session_id)
            .map(|(address, _)| *address)
        else {
            return Ok(());
        };
        let Some(mut session) = sessions.remove(&address) else {
            return Ok(());
        };
        if sleep {
            let sleep_result = session.controller.sleep(session.transport.as_mut()).await;
            if let Err(error) = sleep_result {
                sessions.insert(address, session);
                return Err(error);
            }
        }
        session.report_task.abort();
        let disconnect_result = session.transport.disconnect().await;
        let _ = events.send(ManagerEvent::Disconnected {
            controller_id: session.info.controller_id,
            session_id,
            reason: if sleep {
                "sleep requested"
            } else {
                "disconnected"
            }
            .into(),
            reconnecting: false,
        });
        disconnect_result
    }
}
