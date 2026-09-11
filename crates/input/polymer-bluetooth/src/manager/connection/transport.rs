use super::super::*;

impl BluetoothActor {
    pub(in super::super) async fn connect(
        context: ConnectContext,
        address: DeviceAddr,
        session_id: SessionId,
        options: ConnectOptions,
        device: Discovered,
    ) -> Result<Session> {
        let ConnectContext {
            config,
            host,
            pairing,
            keys,
            internal,
            events,
        } = context;
        let mut identity = device.identity.clone();
        let mut identified_handle = None;
        if identity.transport == TransportKind::BrEdr {
            let preliminary = config.controllers.identify(&identity)?;
            if preliminary
                .as_ref()
                .is_none_or(|controller| controller.id().as_str() == "generic.hid")
            {
                let host = host.as_ref().ok_or(Error::AdapterNotFound)?;
                let inquiry = device
                    .bredr
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BR/EDR inquiry data"))?;
                let handle = host.connect(inquiry).await?;
                match sdp::query_identity(&Link::new(host.clone(), handle)).await {
                    Ok(sdp) => {
                        identity.service_uuids = sdp.services;
                        identity.pnp = sdp.pnp;
                    }
                    Err(error) => {
                        tracing::debug!(%address, %error, "SDP identity query failed; retaining discovery identity")
                    }
                }
                identified_handle = Some(handle);
            }
        }
        let mut controller = config
            .controllers
            .identify(&identity)?
            .ok_or(Error::Unsupported(
                "no controller profile matched the device",
            ))?;
        keys.register(ControllerRecord {
            address,
            transport: identity.transport,
            profile_id: controller.id(),
            link_key: None,
            link_key_type: None,
            adapter_index: (identity.transport == TransportKind::Ble)
                .then_some(config.ble_adapter_index),
            auto_reconnect: options.auto_reconnect,
            player_index: options.player_index,
        })?;
        let controller_id = ControllerId::new(identity.transport, address);
        let _ = events.send(ManagerEvent::Connecting {
            controller_id,
            profile_id: controller.id(),
        });
        let policy = controller.connection_policy(&identity, &options);
        let mut transport: Box<dyn Transport> = match device.identity.transport {
            TransportKind::BrEdr => {
                let host = host.as_ref().ok_or(Error::AdapterNotFound)?;
                let inquiry = device
                    .bredr
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BR/EDR inquiry data"))?;
                host.set_host_identity(
                    policy.host_identity.name,
                    policy.host_identity.class_of_device,
                )
                .await?;
                if let Some(pairing) = pairing.as_ref() {
                    let pin = match policy.pairing {
                        PairingPolicy::FixedPin(pin) => Some(PinCode::Fixed(pin.to_vec())),
                        PairingPolicy::RemoteAddressPin => Some(PinCode::RemoteAddress),
                        PairingPolicy::LocalAddressPin => Some(PinCode::LocalAddress),
                        _ => None,
                    };
                    if let Some(pin) = pin {
                        pairing.set_pin(address, pin)?;
                    }
                }
                let mut hid = BrEdrHid::new(host.clone());
                if let Some(slots) = policy.automatic_flush_slots {
                    hid.set_automatic_flush_slots(slots);
                }
                let connect_result = if let Some(handle) = identified_handle {
                    hid.establish_handle(handle, policy.controller_opens_hid)
                        .await
                } else if policy.controller_opens_hid {
                    hid.connect_discovered_device_initiated_hid(inquiry).await
                } else {
                    hid.connect_discovered(inquiry).await
                };
                let restore_result = host.set_host_identity("Polymer", [0x0c, 0x01, 0x1c]).await;
                connect_result?;
                restore_result?;
                Box::new(hid)
            }
            TransportKind::Ble => {
                let _ble = device
                    .ble
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BLE discovery data"))?;
                let gatt_profile =
                    controller
                        .gatt_profile(&identity)?
                        .ok_or(Error::Unsupported(
                            "controller profile does not provide a BLE GATT configuration",
                        ))?;
                let mut gatt = BleGatt::new(config.ble_adapter_index, gatt_profile);
                gatt.connect(address).await?;
                if matches!(policy.pairing, PairingPolicy::PseudoOob) {
                    let host_address = default_adapter_address().await?;
                    controller.bond(&mut gatt, &identity, host_address).await?;
                }
                Box::new(gatt)
            }
        };
        if let Some(uuid) = transport.input_characteristic_uuid()
            && let Some(refined) = config.controllers.identify_input_characteristic(uuid)?
        {
            controller = refined;
        }
        identity
            .name
            .get_or_insert_with(|| controller.display_name().to_owned());
        let profile_id = controller.id();
        let vendor_product = controller.vendor_product(&identity);
        let mut controller_session = controller.connect(&options);
        if let Err(error) = controller_session.synchronize(transport.as_mut()).await {
            let _ = transport.disconnect().await;
            return Err(error);
        }
        let serial = controller_session
            .serial()
            .or_else(|| controller.serial(&identity));
        keys.register(ControllerRecord {
            address,
            transport: identity.transport,
            profile_id: profile_id.clone(),
            link_key: None,
            link_key_type: None,
            adapter_index: (identity.transport == TransportKind::Ble)
                .then_some(config.ble_adapter_index),
            auto_reconnect: options.auto_reconnect,
            player_index: options.player_index,
        })?;
        let report_task = Self::spawn_reports(
            controller_id,
            session_id,
            transport.reports(),
            transport.disconnections(),
            events.clone(),
            internal,
        );
        let sensor_calibration = controller_session.sensor_calibration();
        let ds3_motion = controller_session.ds3_motion();
        let session = Session {
            transport,
            controller: controller_session,
            report_task,
            externally_owned: false,
            info: ConnectedController {
                sensor_calibration,
                ds3_motion,
                controller_id,
                session_id,
                identity: identity.clone(),
                profile_id,
                vendor_product,
                serial,
                player_index: options.player_index,
            },
        };
        Ok(session)
    }
}

impl BluetoothActor {
    pub(in super::super) async fn accept_inbound(
        config: &ManagerConfig,
        host: &Arc<HciHost>,
        address: DeviceAddr,
        session_id: SessionId,
        events: &broadcast::Sender<ManagerEvent>,
        internal: Internal,
        discovered: Option<&Discovered>,
    ) -> Result<Session> {
        let mut record = config
            .store
            .records()?
            .into_iter()
            .find(|record| {
                record.address == address
                    && record.transport == TransportKind::BrEdr
                    && record.auto_reconnect
            })
            .ok_or(Error::Unsupported(
                "inbound connection is not from a persisted controller",
            ))?;
        let controller = match config.controllers.get(&record.profile_id) {
            Some(controller) => controller,
            None => {
                let identity = &discovered
                    .ok_or_else(|| Error::MissingProfile(record.profile_id.to_string()))?
                    .identity;
                let controller = config
                    .controllers
                    .identify(identity)?
                    .ok_or_else(|| Error::MissingProfile(record.profile_id.to_string()))?;
                record.profile_id = controller.id();
                config.store.put(record.clone())?;
                controller
            }
        };
        let identity = discovered.map_or_else(
            || DeviceIdentity {
                address,
                transport: TransportKind::BrEdr,
                name: None,
                class_of_device: None,
                service_uuids: Vec::new(),
                manufacturer_data: Vec::new(),
                pnp: None,
            },
            |device| device.identity.clone(),
        );
        let options = ConnectOptions {
            player_index: record.player_index,
            auto_reconnect: true,
        };
        let policy = controller.connection_policy(&identity, &options);
        let frames = host.l2cap_frames();
        let connection_events = host.events();
        host.set_host_identity(
            policy.host_identity.name,
            policy.host_identity.class_of_device,
        )
        .await?;
        let handle_result = host
            .accept_connection_with_role(address, policy.become_master)
            .await;
        let restore_result = host.set_host_identity("Polymer", [0x0c, 0x01, 0x1c]).await;
        let handle = handle_result?;
        restore_result?;
        let mut hid = BrEdrHid::new(host.clone());
        if let Some(slots) = policy.automatic_flush_slots {
            hid.set_automatic_flush_slots(slots);
        }
        let attach_result = if policy.host_opens_hid_on_reconnect {
            hid.attach_inbound_host_hid_with_frames(handle, frames).await
        } else if policy.wait_for_encryption_before_inbound_hid {
            hid.attach_inbound_encrypted_with_frames(handle, frames, connection_events)
                .await
        } else {
            hid.attach_inbound_with_frames(handle, frames).await
        };
        if let Err(error) = attach_result {
            let _ = host.disconnect(handle).await;
            return Err(error);
        }
        let mut transport: Box<dyn Transport> = Box::new(hid);
        let vendor_product = controller.vendor_product(&identity);
        let mut controller_session = controller.connect(&options);
        if let Err(error) = controller_session.synchronize(transport.as_mut()).await {
            let _ = transport.disconnect().await;
            return Err(error);
        }
        let serial = controller_session
            .serial()
            .or_else(|| controller.serial(&identity));
        let controller_id = ControllerId::new(TransportKind::BrEdr, address);
        let report_task = Self::spawn_reports(
            controller_id,
            session_id,
            transport.reports(),
            transport.disconnections(),
            events.clone(),
            internal,
        );
        let sensor_calibration = controller_session.sensor_calibration();
        let ds3_motion = controller_session.ds3_motion();
        Ok(Session {
            transport,
            controller: controller_session,
            report_task,
            externally_owned: false,
            info: ConnectedController {
                sensor_calibration,
                ds3_motion,
                controller_id,
                session_id,
                identity: identity.clone(),
                profile_id: record.profile_id.clone(),
                vendor_product,
                serial,
                player_index: record.player_index,
            },
        })
    }
}
