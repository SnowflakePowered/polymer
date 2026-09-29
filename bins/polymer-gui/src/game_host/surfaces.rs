use super::*;

impl GameHost {
    pub fn detach_capture(&mut self) {
        self.capture.detach();
    }

    /// Consumes and applies one capture event. Raw runtime offers and frame
    /// updates never leave the game-host boundary.
    pub fn try_capture_event(&mut self) -> Option<GameHostCaptureEvent> {
        if let Some(offer) = self.pending_offer.take() {
            return Some(self.handle_surface_offer(offer));
        }
        loop {
            let worker = self.compositor.borrow_mut().try_surface_event();
            let Some(worker) = worker else {
                break;
            };
            if let Some((frame, dimensions_changed)) =
                self.update_surface_frame(worker.connection, worker.update)
            {
                return Some(GameHostCaptureEvent::Frame {
                    connection: worker.connection,
                    frame,
                    observed_qpc: Some(worker.observed_qpc),
                    dimensions_changed,
                });
            }
        }
        loop {
            return Some(match self.capture.try_recv().ok()? {
                SurfaceCaptureEvent::Offered(offer) => self.handle_surface_offer(offer),
                SurfaceCaptureEvent::Frame { connection, update } => {
                    let Some((frame, dimensions_changed)) =
                        self.update_surface_frame(connection, update)
                    else {
                        continue;
                    };
                    GameHostCaptureEvent::Frame {
                        connection,
                        frame,
                        observed_qpc: None,
                        dimensions_changed,
                    }
                }
                SurfaceCaptureEvent::Removed { connection, .. } => GameHostCaptureEvent::Removed {
                    connection,
                    layout_changed: self.remove_surface(connection),
                },
            });
        }
    }

    fn handle_surface_offer(
        &mut self,
        offer: polymer_runtime::SurfaceOffer,
    ) -> GameHostCaptureEvent {
        if !self.compositor.borrow().matches_device(&offer) {
            self.pending_offer = Some(offer);
            return GameHostCaptureEvent::DeviceRequired;
        }
        let connection = offer.connection_id();
        match self.accept_surface_offer(offer) {
            Ok(accepted) => GameHostCaptureEvent::Connected(accepted),
            Err(error) => GameHostCaptureEvent::Rejected {
                connection,
                error: error.to_string(),
            },
        }
    }

    /// Selects the offer's adapter, imports it into the window-spanning
    /// compositor, and accepts the capture stream as one atomic host action.
    fn accept_surface_offer(
        &mut self,
        offer: polymer_runtime::SurfaceOffer,
    ) -> Result<Option<ConnectedSurface>, Box<dyn Error>> {
        let Some(surface_capacity) = self
            .active
            .as_ref()
            .map(|active| active.options.surfaces.len())
        else {
            self.capture.reject(offer);
            return Ok(None);
        };
        let connection = offer.connection_id();
        let surface_id = offer.stream().surface_id.0;
        let replaced_connection = offer
            .replaced_connection_id()
            .filter(|connection| self.surfaces.contains_key(connection));
        if replaced_connection.is_none() && self.surfaces.len() >= surface_capacity {
            self.capture.reject(offer);
            return Ok(None);
        }

        let (surface, target_index, quarter_turns) = if let Some(previous) = replaced_connection {
            let surface = self
                .surfaces
                .get(&previous)
                .cloned()
                .ok_or("the replaced tracked surface is no longer connected")?;
            let target_index = surface.index;
            let quarter_turns = surface.quarter_turns;
            (surface, target_index, quarter_turns)
        } else {
            let (target_index, quarter_turns) = self
                .target_configuration(offer.id().0)
                .ok_or("the game host already has this render target")?;
            let stream = offer.stream();
            let (width, height) = rotated_extent(stream.width, stream.height, quarter_turns);
            (
                CapturedSurface {
                    index: target_index,
                    preferred_width: width,
                    preferred_height: height,
                    quarter_turns,
                    geometries: vec![SurfaceGeometry {
                        x: 0,
                        y: 0,
                        width: width.max(1),
                        height: height.max(1),
                        pointer_events: true,
                        z_index: target_index as i32,
                        group: None,
                        corner_radius: 0.0,
                        style: None,
                        opacity: 1.0,
                    }],
                },
                target_index,
                quarter_turns,
            )
        };
        let backend = format!("{:?}", offer.stream().image_kind);
        self.compositor
            .borrow_mut()
            .import_surface(&offer, quarter_turns)?;
        let detached = self.compositor.borrow().accepts_detached_frames();
        let accepted = if detached {
            self.capture
                .accept_detached(offer)
                .map(|(_, frames)| Some(frames))
        } else {
            self.capture.accept(offer).map(|_| None)
        };
        let frames = match accepted {
            Ok(frames) => frames,
            Err(error) => {
                self.compositor.borrow_mut().remove_surface(connection);
                return Err(error.into());
            }
        };
        let registration = {
            let mut compositor = self.compositor.borrow_mut();
            let worker = if let Some(frames) = frames {
                compositor.attach_surface_frames(connection, frames, self.pointer.clone())
            } else {
                Ok(())
            };
            worker.and_then(|()| compositor.register_surface(connection, target_index))
        };
        if let Err(error) = registration {
            self.compositor.borrow_mut().remove_surface(connection);
            self.capture.revoke(connection);
            return Err(error);
        }
        self.surfaces.insert(connection, surface);
        if let Some(previous) = replaced_connection {
            self.surfaces.remove(&previous);
            self.compositor.borrow_mut().remove_surface(previous);
        }
        self.mark_composition_dirty();
        Ok(Some(ConnectedSurface {
            connection,
            surface_id,
            target_index,
            replaced_connection,
            backend,
        }))
    }

    pub(crate) fn replace_render_device(&mut self) -> Result<Vec<u64>, Box<dyn Error>> {
        let offer = self
            .pending_offer
            .as_ref()
            .ok_or("there is no pending surface offer")?;
        self.compositor.borrow_mut().replace_device(offer)?;
        let stale_connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for &connection in &stale_connections {
            self.capture.revoke(connection);
        }
        self.surfaces.clear();
        self.mark_composition_dirty();
        self.shell_refresh_pending = true;
        Ok(stale_connections)
    }

    pub(crate) fn reject_pending_offer(&mut self) -> Option<u64> {
        let offer = self.pending_offer.take()?;
        let connection = offer.connection_id();
        self.capture.reject(offer);
        Some(connection)
    }

    fn update_surface_frame(
        &mut self,
        connection: u64,
        update: polymer_runtime::SurfaceFrameUpdate,
    ) -> Option<(polymer_protocol::FrameReady, bool)> {
        let frame = update.frame;
        let surface = self.surfaces.get_mut(&connection)?;
        let (width, height) = rotated_extent(
            frame.render_width.max(1),
            frame.render_height.max(1),
            surface.quarter_turns,
        );
        let dimensions_changed =
            (surface.preferred_width, surface.preferred_height) != (width, height);
        if dimensions_changed {
            surface.preferred_width = width;
            surface.preferred_height = height;
        }
        self.compositor.borrow_mut().set_latest(connection, frame);
        if dimensions_changed {
            self.mark_composition_dirty();
        }
        Some((frame, dimensions_changed))
    }

    fn remove_surface(&mut self, connection: u64) -> bool {
        self.compositor.borrow_mut().remove_surface(connection);
        let removed = self.surfaces.remove(&connection).is_some();
        if removed {
            self.mark_composition_dirty();
        }
        removed
    }

    pub fn finish_active_game(&mut self) -> Option<FinishedGame> {
        let mut active = self.active.take()?;
        let connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        self.compositor.borrow_mut().stop_webrtc();
        for &connection in &connections {
            self.compositor.borrow_mut().remove_surface(connection);
            self.capture.revoke(connection);
        }
        self.capture.detach();
        self.surfaces.clear();
        self.pending_offer = None;
        self.mark_composition_dirty();
        let token = active.token;
        if let Some(automation) = active.automation.take() {
            automation.close();
        }
        drop(active.controller_session.take());
        let finalization_error = self
            .session
            .finish(polymer_vfs::FinalizationReason::ProcessExited)
            .err();
        Some(FinishedGame {
            token,
            connections,
            finalization_error,
        })
    }

    fn target_configuration(&self, index: u32) -> Option<(u32, u8)> {
        let active = self.active.as_ref()?;
        active
            .options
            .surfaces
            .iter()
            .find(|target| {
                target.index == index
                    && !self
                        .surfaces
                        .values()
                        .any(|surface| surface.index == target.index)
            })
            .map(|target| (target.index, normalized_quarter_turns(target.rotation)))
    }

    pub fn has_pending_frame(&self) -> bool {
        self.compositor.borrow().has_pending_frame()
    }

    pub fn draw(
        &mut self,
        _width: u32,
        _height: u32,
        permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        let corners = self
            .surfaces
            .iter()
            .map(|(&connection, surface)| (connection, surface.primary().corner_radius))
            .collect::<Vec<_>>();
        self.compositor
            .borrow_mut()
            .present_surfaces(&corners, permit_wait)
    }

    pub fn apply_tracked_layout(&mut self, placements: &[TrackedSurfaceLayout]) -> bool {
        let mut placed = HashMap::<u64, Vec<SurfaceGeometry>>::new();
        for placement in placements {
            let Some((&connection, _)) = self
                .surfaces
                .iter()
                .find(|(_, surface)| u64::from(surface.index) == placement.source_id)
            else {
                continue;
            };
            // Keep the guest visual in the tracked element's full coordinate
            // space. Shrinking this rectangle to the accumulated paint clip
            // changes object-fit and relocates the sampled image whenever a
            // panel approaches a viewport edge, while its chrome remains in
            // the original space. The desktop composition root already clips
            // visuals at the window boundary; finer clips belong on the WUC
            // visual itself, not in its layout geometry.
            let rect = placement.bounds;
            placed.entry(connection).or_default().push(SurfaceGeometry {
                x: rect.x0.round() as i32,
                y: rect.y0.round() as i32,
                width: rect.width().round().max(0.0) as u32,
                height: rect.height().round().max(0.0) as u32,
                pointer_events: true,
                z_index: placement.z_order,
                group: placement.group,
                corner_radius: placement.corner_radius,
                style: placement.style.clone(),
                opacity: placement.opacity,
            });
        }

        let requested = self
            .active
            .as_ref()
            .map(|active| {
                active
                    .options
                    .surfaces
                    .iter()
                    .map(|surface| surface.index)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let mut changed = false;
        for (connection, surface) in &mut self.surfaces {
            let geometries = match placed.remove(connection) {
                Some(geometries) => geometries,
                None if requested.contains(&surface.index) => vec![SurfaceGeometry::hidden()],
                None => continue,
            };
            if surface.geometries != geometries {
                surface.geometries = geometries;
                changed = true;
            }
        }
        if changed {
            self.mark_composition_dirty();
        }
        changed
    }
}
