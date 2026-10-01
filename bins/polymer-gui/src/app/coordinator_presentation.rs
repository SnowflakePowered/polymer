use super::*;

impl ShellCoordinator {
    pub(super) fn handle_compositor_tick(&mut self) {
        self.compositor.borrow_mut().clear_clock_tick();
        self.compositor.borrow_mut().record_clock();
        self.refresh_pending_shell();
        self.present_pending_surfaces(PRESENT_PERMIT_WAIT);
        // A DXGI latency handle can remain signalled after the available frame
        // has been consumed. Stop waiting until capture publishes more work so
        // the clock thread cannot flood the UI loop with empty ticks.
        let pending = self.has_pending_surfaces();
        self.compositor.borrow().set_clock_active(pending);
    }

    pub(super) fn process_capture(&mut self) {
        let entered = self.profile.enter();
        self.process_capture_inner();
        self.profile.record(TurnPhase::Capture, entered);
    }

    pub(super) fn process_capture_inner(&mut self) {
        // Leave the notification latched while draining. Producers can mark
        // `capture_dirty`, but cannot flood the event loop with one user event
        // per captured frame.
        self.game_host.begin_capture_drain();
        let mut changed = false;
        let mut shell_refresh_required = false;
        let mut processed = 0;
        while processed < MAX_CAPTURE_EVENTS_PER_TURN {
            let event = self.game_host.try_capture_event();
            let Some(event) = event else {
                break;
            };
            processed += 1;
            match event {
                GameHostCaptureEvent::DeviceRequired => {
                    let replacement = self.game_host.replace_render_device();
                    match replacement {
                        Ok(stale_connections) => {
                            for connection in stale_connections {
                                self.compositor.borrow_mut().remove_target(connection);
                                self.cancel_pointer_connection(connection);
                            }
                            self.pending_layouts.clear();
                            self.shell_dom.invalidate();
                            shell_refresh_required = true;
                            changed = true;
                        }
                        Err(error) => {
                            let connection = self.game_host.reject_pending_offer();
                            tracing::error!(
                                "polymer: replacing the shell compositor device for capture {connection:?} failed: {error}"
                            );
                        }
                    }
                }
                GameHostCaptureEvent::Connected(surface) => {
                    let Some(surface) = surface else {
                        continue;
                    };
                    if let Some(previous) = surface.replaced_connection {
                        self.compositor.borrow_mut().remove_target(previous);
                        self.cancel_pointer_connection(previous);
                    }
                    self.compositor.borrow_mut().set_backend(surface.backend);
                    self.compositor.borrow_mut().add_surface(
                        surface.connection,
                        surface.surface_id,
                        surface.target_index,
                        surface.extent,
                    );
                    shell_refresh_required = true;
                    changed = true;
                }
                GameHostCaptureEvent::Rejected { connection, error } => {
                    tracing::error!("polymer: rejected capture stream {connection}: {error}");
                }
                GameHostCaptureEvent::Frame {
                    connection,
                    frame,
                    observed_qpc,
                } => {
                    self.compositor
                        .borrow_mut()
                        .observe_frame_at(connection, frame, observed_qpc);
                }
                GameHostCaptureEvent::Removed {
                    connection,
                    layout_changed,
                } => {
                    self.compositor.borrow_mut().remove_target(connection);
                    self.cancel_pointer_connection(connection);
                    if layout_changed {
                        changed = true;
                    }
                }
            }
        }
        if shell_refresh_required {
            self.refresh_shell();
        } else {
            self.sync_shell_visuals();
        }
        if changed && let Some(token) = self.game_host.active_token() {
            self.publish_targets_for(token);
        }
        self.draw_unpaced_surfaces();
        // A producer that ran while the notification was latched could not
        // enqueue another WakeCapture. Carry that work into the next event-loop
        // cycle instead. Resetting the latch before reading `capture_dirty`
        // closes the race: a later producer will enqueue a fresh notification.
        self.game_host
            .finish_capture_drain(processed == MAX_CAPTURE_EVENTS_PER_TURN);
    }

    /// Presents pending content only while nothing else is pacing the shell.
    ///
    /// Once the compositor clock is running it owns every present, so that each
    /// one lands at the start of a composition interval rather than wherever
    /// the guest happened to finish a frame. Presenting on arrival puts it at
    /// an arbitrary phase against DWM's sampling deadline, and a guest and a
    /// display at nominally the same rate drift that phase slowly across the
    /// deadline — a stretch of doubled and dropped frames every time it
    /// crosses.
    pub(super) fn draw_unpaced_surfaces(&mut self) {
        if !self.compositor.borrow_mut().is_pacing() {
            self.draw_pending_surfaces();
        }
    }

    pub(super) fn draw_pending_surfaces(&mut self) {
        self.present_pending_surfaces(Duration::ZERO);
    }

    /// Presents every surface holding new content, waiting up to `permit_wait`
    /// for DXGI to hand back a present slot.
    pub(super) fn present_pending_surfaces(&mut self, permit_wait: Duration) {
        let entered = self.profile.enter();
        let timing = std::env::var_os("POLYMER_GAME_LAYER_TIMING")
            .is_some()
            .then(std::time::Instant::now);
        let size = self.compositor.borrow_mut().size();
        let presented = match self.game_host.draw(size.width, size.height, permit_wait) {
            Ok(Some(carried_new_frame)) => {
                self.compositor
                    .borrow_mut()
                    .record_present(carried_new_frame);
                true
            }
            Ok(None) => false,
            Err(error) => {
                tracing::error!("polymer: rendering merged surfaces failed: {error}");
                false
            }
        };
        if let Some(started) = timing {
            let elapsed = started.elapsed();
            tracing::error!(
                "polymer: compositor {} in {:.2}ms (permit wait {:.2}ms)",
                if presented { "presented" } else { "deferred" },
                elapsed.as_secs_f64() * 1000.0,
                permit_wait.as_secs_f64() * 1000.0,
            );
        }
        self.profile.record(TurnPhase::Present, entered);
    }

    pub(super) fn has_pending_surfaces(&self) -> bool {
        // Re-presenting an unchanged shared buffer burns GPU time and can
        // occupy the one-frame DXGI queue when a real guest frame arrives.
        // DirectComposition retains the last buffer, so only new content or
        // chrome needs another present.
        self.game_host.has_pending_frame()
    }

    pub(super) fn draw_pending_chrome(&mut self) {
        let publication = self.compositor.borrow_mut().publish_scene();
        match publication {
            Ok(Some(revision)) => {
                if let Some(placements) = self.pending_layouts.remove(&revision) {
                    self.game_host.apply_tracked_layout(&placements);
                }
                self.pending_layouts
                    .retain(|pending, _| *pending > revision);
            }
            Ok(None) => {}
            Err(error) => tracing::error!("polymer: presenting shell chrome failed: {error}"),
        }
        self.sync_shell_visuals();
        if self.compositor.borrow().scene_pending() {
            self.compositor.borrow_mut().request_redraw();
        }
    }

    fn sync_shell_visuals(&mut self) {
        let layers = self.game_host.captured_surface_snapshots();
        let revision = self.game_host.composition_revision();
        let reconciled = self.compositor.borrow_mut().reconcile(&layers, revision);
        match reconciled {
            Ok(true) => {
                if let Err(error) = self.compositor.borrow_mut().commit() {
                    tracing::error!("polymer: committing shell visuals failed: {error}");
                }
            }
            Ok(false) => {}
            Err(error) => tracing::error!("polymer: obtaining shell visuals failed: {error}"),
        }
    }

    /// Re-lays out the shell document and moves the compositor to match.
    pub(super) fn refresh_shell(&mut self) {
        let entered = self.profile.enter();

        self.game_host.clear_shell_refresh_request();
        let Some(frame) = self.shell_dom.produce_frame() else {
            return;
        };
        if let Some((inspector, revision)) = self
            .inspector
            .as_ref()
            .zip(self.shell_dom.inspector_structure_revision())
        {
            inspector.document_updated(revision);
        }
        self.next_scene_revision += 1;
        let revision = self.next_scene_revision;
        self.pending_layouts.insert(revision, frame.surface_layout);
        let superseded = self.compositor.borrow_mut().submit_scene(CompositionScene {
            revision,
            raster_chunks: frame.raster_chunks,
            composition_groups: frame.composition_groups,
            width: frame.width,
            height: frame.height,
        });
        if let Some(superseded) = superseded {
            self.pending_layouts.remove(&superseded);
        }
        self.draw_pending_chrome();

        self.profile.record(TurnPhase::Shell, entered);
    }
}
