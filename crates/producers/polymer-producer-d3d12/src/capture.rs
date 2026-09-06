//! Swapchain capture state, copy submission, and viewer handoff.

use crate::{D3d12SwapchainCapture, Tracking};
use parking_lot::Mutex;
use polymer_hook_dxgi::{SwapchainOwner, SwapchainOwnerHandle};
use polymer_platform_windows::performance_counter;
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::FramePublisher;
use polymer_presentation::producer::ProducerSession;
use polymer_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, PresentationTransform, StreamId, SurfaceId,
    transport_unorm_dxgi_format,
};
use std::{ffi::c_void, sync::Arc};
use windows::{
    Win32::Graphics::Direct3D12::ID3D12CommandQueue,
    core::{IUnknown, Interface},
};

struct CaptureState {
    capture: D3d12SwapchainCapture,
    ipc: Option<ProducerSession>,
}

/// The capture ring for one swapchain. A ring is created lazily on the first
/// present and dropped when the swapchain resizes; a swapchain the viewer
/// declined stays parked until it resizes or is destroyed, so a rejected
/// surface does not reconnect on every present.
type Capture =
    polymer_presentation::producer::CaptureSession<CaptureState, (usize, Option<(u32, u32, u32)>)>;

/// Per-swapchain capture state stored in the swapchain's hook context.
///
/// The queue and device tracking are fixed at claim time; the ring behind the
/// mutex is created on demand and released across resizes.
pub struct SwapchainCapture {
    stream_id: u64,
    queue: ID3D12CommandQueue,
    tracking: Arc<Tracking>,
    capture: Mutex<Vec<Capture>>,
    history: Mutex<polymer_presentation::history::ResourceHistory<crate::tracking::Presentation>>,
}

/// Claims a DXGI swapchain created for a D3D12 command queue, returning the
/// per-swapchain owner the central hook drives from its trampolines.
///
/// # Safety
///
/// `swapchain` and `creation_device` must point to live COM interfaces.
pub unsafe fn claim(
    swapchain: *mut c_void,
    creation_device: *mut c_void,
    tracking: Arc<Tracking>,
) -> Option<SwapchainOwnerHandle> {
    if creation_device.is_null() {
        return None;
    }
    let stream_id = unsafe { swapchain_key(swapchain) }? as u64;
    let queue = unsafe { IUnknown::from_raw_borrowed(&creation_device) }?
        .cast::<ID3D12CommandQueue>()
        .ok()?;
    Some(SwapchainOwnerHandle::new(SwapchainCapture {
        stream_id,
        queue,
        tracking,
        capture: Mutex::new(Vec::new()),
        history: Mutex::new(Default::default()),
    }))
}

impl SwapchainOwner for SwapchainCapture {
    /// Captures the image that is about to be presented.
    ///
    /// # Safety
    ///
    /// `swapchain` must point to the live swapchain being presented.
    unsafe fn before_present(&self, swapchain: *mut c_void) {
        let _capture_scope = polymer_presentation::observation::CaptureScope::enter();
        let requested =
            polymer_hook::config::tracked_render_target_indices().unwrap_or_else(|| vec![0]);
        let limit = requested.iter().copied().max().map_or(1, |index| index + 1);
        let presentations = if polymer_hook::config::tracking_enabled() {
            unsafe {
                self.tracking.swapchain_presentations(
                    swapchain,
                    limit,
                    self.queue.as_raw() as usize,
                )
            }
        } else {
            Vec::new()
        };
        let exact = !presentations.is_empty()
            && presentations
                .iter()
                .all(|presentation| presentation.exact_selection);
        let reconciled = self
            .history
            .lock()
            .reconcile(presentations, exact, |presentation| {
                self.tracking
                    .refresh_presentation(presentation, self.queue.as_raw() as usize)
            });
        let presentations = reconciled.surfaces;
        let now = self
            .tracking
            .presentation_clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let window = unsafe {
            windows::Win32::Graphics::Dxgi::IDXGISwapChain::from_raw_borrowed(&swapchain)
        }
        .and_then(|swapchain| unsafe { swapchain.GetDesc().ok() })
        .map_or(0, |desc| desc.OutputWindow.0 as isize);
        use polymer_presentation::history::HistorySurface;
        polymer_presentation::producer::ProducerWindows::refresh(&self.tracking.registry);
        use polymer_presentation::registry::{RegistrySource, SourceIdentity};
        let sources = presentations
            .iter()
            .zip(&reconciled.slots)
            .map(|(presentation, &logical_slot)| {
                let (resource, subresource) = presentation.identity();
                let (width, height, _, _) = presentation.layout();
                RegistrySource {
                    logical_slot,
                    identity: SourceIdentity {
                        domain: 0,
                        resource: if resource == 0 {
                            self.stream_id
                        } else {
                            resource as u64
                        },
                        subresource,
                    },
                    extent: polymer_presentation::Extent2D { width, height },
                    placement: None,
                }
            })
            .collect();
        self.tracking
            .registry
            .observe(self.stream_id, window, sources, now);
        let resolved = if polymer_hook::config::tracking_enabled() && !presentations.is_empty() {
            self.tracking
                .registry
                .requests(self.stream_id, &requested, now, |slot, window| {
                    let title = polymer_platform_windows::window_title(
                        polymer_platform_windows::NativeWindow(window),
                    );
                    polymer_hook::config::tracked_render_target_surface_matches(
                        slot,
                        title.as_deref().unwrap_or_default(),
                    )
                })
        } else {
            vec![(0, 0)]
        };
        let mut captures = self.capture.lock();
        captures.resize_with(requested.len(), Capture::default);
        captures.truncate(requested.len());
        for change in reconciled.changes {
            let source_slot = match change {
                polymer_presentation::history::SurfaceChange::Replace { slot }
                | polymer_presentation::history::SurfaceChange::Add { slot } => slot,
                _ => continue,
            };
            for &(slot, index) in &resolved {
                if reconciled.slots.get(index) == Some(&source_slot) {
                    captures[slot].source_changed();
                }
            }
        }
        for (slot, capture) in captures.iter_mut().enumerate() {
            if !resolved.iter().any(|(selected, _)| *selected == slot) {
                capture.park();
            }
        }
        for (slot, index) in resolved {
            let presentation = presentations.get(index);
            if presentation.is_none() && (!presentations.is_empty() || slot != 0) {
                captures[slot].invalidate();
                continue;
            }
            unsafe {
                self.capture_source(
                    swapchain,
                    presentation,
                    slot,
                    requested[slot],
                    &mut captures[slot],
                )
            };
        }
    }

    /// Releases the capture ring and tracking references before the swapchain
    /// buffers resize.
    ///
    /// # Safety
    ///
    /// `swapchain` must point to the live swapchain being resized.
    unsafe fn before_resize(&self, swapchain: *mut c_void) {
        self.history.lock().reset();
        let previous = std::mem::take(&mut *self.capture.lock());
        tracing::debug!(
            "polymer: D3D12 resize released capture state: {}",
            previous.iter().any(|capture| capture.as_ref().is_some())
        );
        unsafe { self.tracking.before_swapchain_resize(swapchain) };
    }
}

impl SwapchainCapture {
    unsafe fn capture_source(
        &self,
        swapchain: *mut c_void,
        presentation: Option<&crate::tracking::Presentation>,
        slot: usize,
        requested_index: usize,
        capture: &mut Capture,
    ) {
        let tracked_source = presentation
            .as_ref()
            .and_then(|value| value.source.as_ref())
            .filter(|source| {
                source.sample_count == 1
                    && source.state.is_some_and(|state| {
                        state.capture_supported(self.tracking.enhanced_barriers_supported())
                    })
            });
        // A tracked source is copied in its own coordinate system. The draw
        // region belongs to the destination backbuffer and is carried only as
        // presentation metadata for pointer mapping.
        let copy_region = presentation.and_then(|value| {
            if tracked_source.is_some() {
                value.source_region
            } else {
                value.region
            }
        });
        capture.configure((
            requested_index,
            tracked_source.map(|source| {
                (
                    source.width,
                    source.height,
                    transport_unorm_dxgi_format(source.format),
                )
            }),
        ));
        let setup = CaptureSetup {
            owner: self,
            swapchain,
            tracked_source,
            slot,
        };
        if let Err(error) = unsafe { capture.initialize(std::time::Instant::now(), &setup) } {
            tracing::debug!("polymer: capture initialization deferred: {error}");
            return;
        }
        let _ = unsafe {
            capture.capture(CopyFrame {
                presentation,
                tracked_source,
                copy_region,
                stream_id: StreamId(self.stream_id ^ ((slot as u64) << 48)),
            })
        };
    }
}

unsafe fn swapchain_key(swapchain: *mut c_void) -> Option<usize> {
    let unknown = unsafe { IUnknown::from_raw_borrowed(&swapchain) }?;
    unknown
        .cast::<IUnknown>()
        .ok()
        .map(|identity| identity.as_raw() as usize)
}

fn connect_viewer(
    capture: &D3d12SwapchainCapture,
    slot: usize,
    mut socket: ProducerSession,
) -> std::io::Result<ProducerSession> {
    let image_handles = capture
        .slots()
        .iter()
        .map(|slot| slot.raw_handle() as i64)
        .collect::<Vec<_>>();
    let desc = capture.desc();
    let (adapter_luid_low, adapter_luid_high) = capture.adapter_luid();
    socket.publish(ExportDescriptor {
        surface_id: SurfaceId(if polymer_hook::config::tracking_enabled() {
            polymer_presentation::registry::PresentationRegistry::logical_surface_id(slot)
        } else {
            capture.source_window() as u64
        }),
        adapter_luid: AdapterLuid {
            low_part: adapter_luid_low,
            high_part: adapter_luid_high,
        },
        source_window: capture.source_window() as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind: ExternalImageKind::D3d12Resource,
        sync_kind: ExternalSyncKind::D3d12Fence,
        image_handles: &image_handles,
        timeline_semaphore_handle: (capture.raw_fence_handle()) as i64,
        consumer_semaphore_handle: (capture.raw_consumer_fence_handle()) as i64,
        bottom_up: false,
    })?;
    Ok(socket)
}

impl Drop for SwapchainCapture {
    fn drop(&mut self) {
        self.tracking.registry.forget_owner(self.stream_id);
    }
}

impl polymer_presentation::producer::CaptureBackend for CaptureState {
    fn connected(&self) -> bool {
        self.ipc.as_ref().is_some_and(ProducerSession::connected)
    }
}

struct CaptureSetup<'a> {
    owner: &'a SwapchainCapture,
    swapchain: *mut c_void,
    tracked_source: Option<&'a crate::tracking::TrackedResource>,
    slot: usize,
}
impl polymer_presentation::producer::CaptureInitializer for CaptureSetup<'_> {
    type Backend = CaptureState;
    type Error = String;
    unsafe fn initialize(&self) -> Result<Option<CaptureState>, String> {
        let Self {
            swapchain,
            tracked_source,
            slot,
            ..
        } = *self;
        let name = polymer_hook::config::session().map_err(|error| error.to_string())?;
        let Some(connection) = ProducerSession::setup_result(ProducerSession::connect(&name))
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        tracing::debug!("polymer: creating D3D12 shared capture ring");
        let ring = match unsafe {
            D3d12SwapchainCapture::new(
                swapchain,
                self.owner.queue.as_raw(),
                3,
                tracked_source.map(|source| (&source.resource, source.subresource)),
                self.owner.tracking.enhanced_barriers_supported(),
            )
        } {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!("polymer: D3D12 capture creation failed: {error}");
                return Err("capture initialization failed".into());
            }
        };
        tracing::debug!("polymer: connecting D3D12 capture ring to viewer");
        let ipc = ProducerSession::setup_result(connect_viewer(&ring, slot, connection))
            .map_err(|error| error.to_string())?;
        if ipc.is_none() {
            return Ok(None);
        }
        let desc = ring.desc();
        tracing::debug!(
            "polymer: attached D3D12 {}x{} {}-slot shared texture ring",
            desc.width,
            desc.height,
            desc.slots
        );
        Ok(Some(CaptureState { capture: ring, ipc }))
    }
}

struct CopyFrame<'a> {
    presentation: Option<&'a crate::tracking::Presentation>,
    tracked_source: Option<&'a crate::tracking::TrackedResource>,
    copy_region: Option<crate::D3d12CaptureRegion>,
    stream_id: StreamId,
}
impl polymer_presentation::producer::CaptureOperation for CopyFrame<'_> {
    type Backend = CaptureState;
    type Output = ();
    type Error = windows::core::Error;
    unsafe fn submit(self, state: &mut CaptureState) -> Result<Option<()>, Self::Error> {
        let Self {
            presentation,
            tracked_source,
            copy_region,
            ..
        } = self;
        let submission = match unsafe {
            state.capture.copy_frame(
                copy_region,
                tracked_source.map(|source| {
                    (
                        &source.resource,
                        source.state.expect("tracked source state was filtered"),
                        source.subresource,
                    )
                }),
            )
        } {
            Ok(Some(submission)) => submission,
            Ok(None) => return Ok(None),
            Err(error) => return Err(error),
        };

        let capture_desc = state.capture.desc();
        let presentation_extent = presentation
            .as_ref()
            .map_or((capture_desc.width, capture_desc.height), |value| {
                value.target_extent
            });
        let presentation_rect = presentation.as_ref().and_then(|value| value.region).map_or(
            (
                submission.capture_x,
                submission.capture_y,
                submission.width,
                submission.height,
            ),
            |region| (region.x, region.y, region.width, region.height),
        );
        let message = polymer_presentation::producer::CapturedFrame {
            stream_id: self.stream_id,
            slot: submission.slot,
            sync_value: submission.sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
            extent: polymer_presentation::Extent2D {
                width: submission.width,
                height: submission.height,
            },
        }
        .present(polymer_presentation::producer::FramePlacement {
            extent: polymer_presentation::Extent2D {
                width: presentation_extent.0,
                height: presentation_extent.1,
            },
            bounds: polymer_presentation::Rect {
                x: (presentation_rect.0) as i32,
                y: (presentation_rect.1) as i32,
                extent: polymer_presentation::Extent2D {
                    width: presentation_rect.2,
                    height: presentation_rect.3,
                },
            },
            transform: if tracked_source.is_some()
                && presentation.is_some_and(|value| !value.placement_known)
            {
                PresentationTransform::UNAVAILABLE
            } else {
                PresentationTransform::axis_aligned(
                    presentation_rect.0,
                    presentation_rect.1,
                    presentation_rect.2,
                    presentation_rect.3,
                )
            },
        });
        state.ipc.publish_frame(message);
        Ok(Some(()))
    }
}
