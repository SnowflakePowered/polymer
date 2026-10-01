use super::*;

struct PendingOffer {
    connection: u64,
    surface_id: TrackedSurfaceId,
    stream: StreamReady,
    accepted: TxOneshot<bool>,
    handle: TrackedSurfaceHandle,
    snapshot: SurfaceSnapshot,
    frames: Watchable<Option<FrameReady>>,
    replacing: Option<u64>,
}

pub(super) struct ActiveSurface {
    connection: u64,
    protocol_surface_id: u64,
    pub(super) handle: TrackedSurfaceHandle,
}

pub(crate) struct SurfaceManager {
    hooks: Arc<[GraphicsHook]>,
    tracked_surfaces: Arc<[TrackedSurfaceConfig]>,
    guest: Weak<Mutex<Option<Box<dyn Guest>>>>,
    router: RwLock<Option<FrameRouter>>,
    events: SurfaceEventSender,
    pub(super) active: DashMap<TrackedSurfaceId, ActiveSurface>,
    pending: DashMap<u64, PendingOffer>,
    topology: Mutex<()>,
    next_offer: AtomicU64,
    remaining_skips: AtomicU32,
    skipped_surfaces: DashSet<(u64, GraphicsHook)>,
    stopping: AtomicBool,
}

impl SurfaceManager {
    pub(super) fn new(
        config: &EmulatorLaunchConfig,
        guest: Weak<Mutex<Option<Box<dyn Guest>>>>,
        events: SurfaceEventSender,
    ) -> Self {
        Self {
            hooks: Arc::from(config.hooks.graphics.clone()),
            tracked_surfaces: Arc::from(config.tracked_surfaces.clone()),
            guest,
            router: RwLock::new(None),
            events,
            active: DashMap::new(),
            pending: DashMap::new(),
            topology: Mutex::new(()),
            next_offer: AtomicU64::new(1),
            remaining_skips: AtomicU32::new(config.skip_surfaces),
            skipped_surfaces: DashSet::new(),
            stopping: AtomicBool::new(false),
        }
    }

    pub(super) fn install_router(&self, router: FrameRouter) {
        *self.router.write() = Some(router);
    }

    pub(super) fn router(&self) -> Option<FrameRouter> {
        self.router.read().clone()
    }

    pub(crate) fn offer(
        self: &Arc<Self>,
        connection: u64,
        stream: StreamReady,
        accepted: TxOneshot<bool>,
    ) {
        let _topology = self.topology.lock();
        if self.stopping.load(Ordering::Acquire) {
            accepted.send(false);
            return;
        }
        let Some(router) = self.router() else {
            accepted.send(false);
            return;
        };
        let backend = stream.hook;
        let Some(incoming_rank) = self.hooks.iter().position(|hook| *hook == backend) else {
            accepted.send(false);
            return;
        };
        let skip_identity = (stream.surface_id.0, backend);
        let remaining_skips = self.remaining_skips.load(Ordering::Acquire);
        if self.skipped_surfaces.contains(&skip_identity) || remaining_skips != 0 {
            if !self.skipped_surfaces.contains(&skip_identity) {
                self.remaining_skips
                    .store(remaining_skips - 1, Ordering::Release);
                if stream.surface_id.0 != 0 {
                    self.skipped_surfaces.insert(skip_identity);
                }
            }
            accepted.send(false);
            return;
        }
        let existing = self
            .active
            .iter()
            .find(|active| {
                stream.surface_id.0 != 0
                    && active.value().protocol_surface_id == stream.surface_id.0
            })
            .map(|active| {
                (
                    *active.key(),
                    active.value().connection,
                    active.value().handle.backend(),
                )
            });
        let pending = self
            .pending
            .iter()
            .find(|pending| {
                stream.surface_id.0 != 0
                    && pending.value().stream.surface_id.0 == stream.surface_id.0
            })
            .map(|pending| {
                (
                    *pending.key(),
                    pending.value().surface_id,
                    pending.value().stream.hook,
                )
            });
        let (surface_id, replacing) = if let Some((offer, id, old_backend)) = pending {
            let old_rank = self
                .hooks
                .iter()
                .position(|hook| *hook == old_backend)
                .unwrap_or(usize::MAX);
            if incoming_rank > old_rank {
                accepted.send(false);
                return;
            }
            self.reject_pending_locked(offer);
            let replacing = self.active.get(&id).map(|active| active.connection);
            (id, replacing)
        } else if let Some((id, old_connection, old_backend)) = existing {
            let old_rank = self
                .hooks
                .iter()
                .position(|hook| *hook == old_backend)
                .unwrap_or(usize::MAX);
            if incoming_rank > old_rank {
                accepted.send(false);
                return;
            }
            (id, Some(old_connection))
        } else {
            let Some(id) = self.unassigned_surface(&stream) else {
                accepted.send(false);
                return;
            };
            (id, None)
        };
        let generation = self
            .active
            .get(&surface_id)
            .map_or(1, |active| active.value().handle.snapshot().generation + 1);
        let frames = Watchable::new(None);
        let snapshot = SurfaceSnapshot {
            generation,
            protocol_surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            backend,
            active: false,
        };
        let handle = self
            .active
            .get(&surface_id)
            .map(|active| active.value().handle.clone())
            .unwrap_or_else(|| TrackedSurfaceHandle {
                id: surface_id,
                shared: Arc::new(SurfaceShared {
                    snapshot: RwLock::new(snapshot.clone()),
                    guest: self.guest.clone(),
                    frames: RwLock::new(frames.clone()),
                }),
            });
        router.route(connection, frames.clone());
        let offer_id = self.next_offer.fetch_add(1, Ordering::AcqRel);
        self.pending.insert(
            offer_id,
            PendingOffer {
                connection,
                surface_id,
                stream,
                accepted,
                handle: handle.clone(),
                snapshot,
                frames,
                replacing,
            },
        );
        let offer = SurfaceOffer {
            offer: offer_id,
            connection,
            stream,
            surface: handle.clone(),
            replacing,
            manager: Arc::downgrade(self),
            resolved: false,
        };
        if self.events.send(SurfaceEvent::Offered(offer)).is_err() {
            self.reject_pending_locked(offer_id);
        } else {
            router.wake();
        }
    }

    pub(super) fn resolve_offer(&self, offer: u64, accept: bool) -> Result<(), SurfaceError> {
        let _topology = self.topology.lock();
        let Some((_, pending)) = self.pending.remove(&offer) else {
            return Err(SurfaceError::Unavailable);
        };
        let Some(router) = self.router() else {
            pending.frames.shutdown();
            pending.accepted.send(false);
            return Err(SurfaceError::Unavailable);
        };
        if !accept || self.stopping.load(Ordering::Acquire) {
            pending.frames.shutdown();
            router.revoke(pending.connection);
            pending.accepted.send(false);
            return if accept {
                Err(SurfaceError::Unavailable)
            } else {
                Ok(())
            };
        }
        if let Some(replaced) = pending.replacing {
            router.revoke(replaced);
        }
        let mut snapshot = pending.snapshot;
        snapshot.active = true;
        *pending.handle.shared.snapshot.write() = snapshot;
        if pending.replacing.is_some() {
            let previous_frames =
                std::mem::replace(&mut *pending.handle.shared.frames.write(), pending.frames);
            previous_frames.shutdown();
        }
        self.active.insert(
            pending.surface_id,
            ActiveSurface {
                connection: pending.connection,
                protocol_surface_id: pending.stream.surface_id.0,
                handle: pending.handle.clone(),
            },
        );
        pending.accepted.send(true);
        Ok(())
    }

    fn reject_pending_locked(&self, offer: u64) {
        let Some((_, pending)) = self.pending.remove(&offer) else {
            return;
        };
        pending.frames.shutdown();
        if let Some(router) = self.router() {
            router.revoke(pending.connection);
        }
        pending.accepted.send(false);
    }

    pub(crate) fn disconnected(&self, connection: u64) {
        let _topology = self.topology.lock();
        let removed = self
            .active
            .iter()
            .find_map(|active| (active.value().connection == connection).then_some(*active.key()));
        let Some(id) = removed else {
            return;
        };
        if let Some((_, active)) = self.active.remove(&id) {
            active.handle.shared.snapshot.write().active = false;
            active.handle.shared.frames.read().shutdown();
        }
        let _ = self.events.send(SurfaceEvent::Removed { id, connection });
        if let Some(router) = self.router() {
            router.wake();
        }
    }

    fn unassigned_surface(&self, stream: &StreamReady) -> Option<TrackedSurfaceId> {
        let index = logical_surface_index(stream.surface_id.0)?;
        self.tracked_surfaces
            .iter()
            .find(|surface| {
                surface.render_target_index == index
                    && !self.active.contains_key(&surface.id)
                    && !self
                        .pending
                        .iter()
                        .any(|pending| pending.value().surface_id == surface.id)
            })
            .map(|surface| surface.id)
    }

    pub(super) fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let _topology = self.topology.lock();
            let pending = self
                .pending
                .iter()
                .map(|pending| *pending.key())
                .collect::<Vec<_>>();
            for offer in pending {
                self.reject_pending_locked(offer);
            }
            let active = self
                .active
                .iter()
                .map(|active| *active.key())
                .collect::<Vec<_>>();
            for id in active {
                let Some((_, active)) = self.active.remove(&id) else {
                    continue;
                };
                if let Some(router) = self.router() {
                    router.revoke(active.connection);
                }
                active.handle.shared.snapshot.write().active = false;
                active.handle.shared.frames.read().shutdown();
                let _ = self.events.send(SurfaceEvent::Removed {
                    id,
                    connection: active.connection,
                });
            }
            if let Some(router) = self.router() {
                router.wake();
            }
        }
        if let Some(router) = self.router() {
            router.shutdown();
        }
    }
}

fn logical_surface_index(surface_id: u64) -> Option<u32> {
    const TAG: u64 = 0x5347_0000_0000_0000;
    const MASK: u64 = 0xffff_0000_0000_0000;
    (surface_id & MASK == TAG)
        .then_some(surface_id & !MASK)
        .and_then(|index| u32::try_from(index).ok())
}
