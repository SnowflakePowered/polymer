use super::*;

impl Handler<SendRaw> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(
        &mut self,
        SendRaw { session_id, bytes }: SendRaw,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let Some(mut session) = self.take_session(session_id) else {
            return AtomicResponse::new(Box::pin(fut::ready(Err(Error::Unsupported(
                "controller session is not connected",
            )))));
        };
        AtomicResponse::new(Box::pin(
            async move {
                let result = session
                    .controller
                    .send_raw_hid(session.transport.as_mut(), &bytes)
                    .await;
                (session, result)
            }
            .into_actor(self)
            .map(move |(session, result), actor, _| {
                actor
                    .sessions
                    .insert(session.info.controller_id.address, session);
                result
            }),
        ))
    }
}

impl Handler<SetExternalOwnership> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(
        &mut self,
        SetExternalOwnership {
            session_id,
            enabled,
        }: SetExternalOwnership,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let Some(mut session) = self.take_session(session_id) else {
            return AtomicResponse::new(Box::pin(fut::ready(Err(Error::Unsupported(
                "controller session is not connected",
            )))));
        };
        AtomicResponse::new(Box::pin(
            async move {
                let result = session
                    .controller
                    .set_external_ownership(session.transport.as_mut(), enabled)
                    .await;
                (session, result)
            }
            .into_actor(self)
            .map(move |(mut session, result), actor, _| {
                if result.is_ok() {
                    session.externally_owned = enabled;
                }
                actor
                    .sessions
                    .insert(session.info.controller_id.address, session);
                result
            }),
        ))
    }
}

impl Handler<SetPlayerIndex> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(
        &mut self,
        SetPlayerIndex { session_id, index }: SetPlayerIndex,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let Some(mut session) = self.take_session(session_id) else {
            return AtomicResponse::new(Box::pin(fut::ready(Err(Error::Unsupported(
                "controller session is not connected",
            )))));
        };
        AtomicResponse::new(Box::pin(
            async move {
                let result = if session.externally_owned {
                    Ok(())
                } else {
                    session
                        .controller
                        .set_player_led(session.transport.as_mut(), index)
                        .await
                };
                (session, result)
            }
            .into_actor(self)
            .map(move |(mut session, result), actor, _| {
                if result.is_ok() && !session.externally_owned {
                    session.info.player_index = index;
                    if let Err(error) = Self::persist_session(&actor.config, &session) {
                        let _ = actor.events.send(ManagerEvent::Error {
                            address: Some(session.info.controller_id.address),
                            message: format!("failed to persist player index: {error}"),
                        });
                    }
                }
                actor
                    .sessions
                    .insert(session.info.controller_id.address, session);
                result
            }),
        ))
    }
}

impl Handler<SetRumble> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(
        &mut self,
        SetRumble {
            session_id,
            low_frequency,
            high_frequency,
        }: SetRumble,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let Some(mut session) = self.take_session(session_id) else {
            return AtomicResponse::new(Box::pin(fut::ready(Err(Error::Unsupported(
                "controller session is not connected",
            )))));
        };
        AtomicResponse::new(Box::pin(
            async move {
                let result = if session.externally_owned {
                    Ok(())
                } else {
                    session
                        .controller
                        .set_rumble(session.transport.as_mut(), low_frequency, high_frequency)
                        .await
                };
                (session, result)
            }
            .into_actor(self)
            .map(move |(session, result), actor, _| {
                actor
                    .sessions
                    .insert(session.info.controller_id.address, session);
                result
            }),
        ))
    }
}

impl Handler<SetIrCamera> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(
        &mut self,
        SetIrCamera { session_id, setup }: SetIrCamera,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let Some(mut session) = self.take_session(session_id) else {
            return AtomicResponse::new(Box::pin(fut::ready(Err(Error::Unsupported(
                "controller session is not connected",
            )))));
        };
        AtomicResponse::new(Box::pin(
            async move {
                let result = if session.externally_owned {
                    Ok(())
                } else {
                    session
                        .controller
                        .set_ir_camera(session.transport.as_mut(), setup)
                        .await
                };
                (session, result)
            }
            .into_actor(self)
            .map(move |(session, result), actor, _| {
                actor
                    .sessions
                    .insert(session.info.controller_id.address, session);
                result
            }),
        ))
    }
}

impl Handler<Sleep> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(&mut self, Sleep { session_id }: Sleep, _: &mut Context<Self>) -> Self::Result {
        self.disconnect_session(session_id, true)
    }
}

impl Handler<Disconnect> for BluetoothActor {
    type Result = AtomicResponse<Self, Result<()>>;
    fn handle(
        &mut self,
        Disconnect { session_id }: Disconnect,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.disconnect_session(session_id, false)
    }
}

impl BluetoothActor {
    fn take_session(&mut self, session_id: SessionId) -> Option<Session> {
        let address = self
            .sessions
            .iter()
            .find(|(_, session)| session.info.session_id == session_id)
            .map(|(address, _)| *address)?;
        self.sessions.remove(&address)
    }

    fn disconnect_session(
        &mut self,
        session_id: SessionId,
        sleep: bool,
    ) -> AtomicResponse<Self, Result<()>> {
        let Some(mut session) = self.take_session(session_id) else {
            return AtomicResponse::new(Box::pin(fut::ready(Ok(()))));
        };
        let controller_id = session.info.controller_id;
        let events = self.events.clone();
        AtomicResponse::new(Box::pin(
            async move {
                if sleep
                    && let Err(error) = session.controller.sleep(session.transport.as_mut()).await
                {
                    return (Some(session), Err(error));
                }
                session.stop_reports();
                let result = session.transport.disconnect().await;
                let _ = events.send(ManagerEvent::Disconnected {
                    controller_id,
                    session_id,
                    reason: if sleep {
                        "sleep requested"
                    } else {
                        "disconnected"
                    }
                    .into(),
                    reconnecting: false,
                });
                (None, result)
            }
            .into_actor(self)
            .map(move |(session, result), actor, _| {
                if let Some(session) = session {
                    actor.sessions.insert(controller_id.address, session);
                }
                if sleep
                    && result.is_ok()
                    && let Some(record) = actor
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
                    Self::arm_ble_wake(
                        record,
                        actor.config.controllers.clone(),
                        actor.internal.clone(),
                        actor.wake_gate.clone(),
                        &mut actor.wake_tasks,
                    );
                }
                result
            }),
        ))
    }
}

impl Handler<SessionEnded> for BluetoothActor {
    type Result = ();
    fn handle(
        &mut self,
        SessionEnded {
            controller_id,
            session_id,
            reason,
        }: SessionEnded,
        _: &mut Context<Self>,
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
            if let Some(actor) = internal.upgrade() {
                actor.do_send(SessionEnded {
                    controller_id,
                    session_id,
                    reason,
                });
            }
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
