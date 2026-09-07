//! Swapchain capture state and viewer handoff.

use crate::{D3d11RingMode, D3d11SwapchainCapture, Tracking};
use parking_lot::Mutex;
use polymer_hook_dxgi::{SwapchainOwner, SwapchainOwnerHandle};
use polymer_platform_windows::performance_counter;
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::ProducerSession;
use polymer_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, StreamId, SurfaceId,
    transport_unorm_dxgi_format,
};
use std::{ffi::c_void, sync::Arc};
use windows::{
    Win32::Graphics::Direct3D11::ID3D11Device,
    core::{IUnknown, Interface},
};

struct CaptureState {
    capture: D3d11SwapchainCapture,
    ipc: Option<ProducerSession>,
}

/// The capture ring for one swapchain. A ring is created lazily on the first
/// present and dropped when the swapchain resizes; a swapchain the viewer
/// declined stays parked until it resizes or is destroyed, so a rejected
/// surface does not reconnect on every present.
#[derive(Default)]
struct Capture {
    native: Option<CaptureState>,
    requirements: Option<(usize, Option<SourceRequirements>)>,
    retry: polymer_presentation::producer::ConnectionRetry,
}
impl Capture {
    fn invalidate(&mut self) {
        self.native = None;
        self.retry = Default::default();
    }
    fn park(&mut self) {
        self.native = None;
        self.retry = polymer_presentation::producer::ConnectionRetry::Parked;
    }
    fn source_changed(&mut self) {
        self.retry.source_changed();
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct SourceRequirements {
    extent: (u32, u32),
    format: u32,
}

impl SourceRequirements {
    pub(crate) fn from_source(source: &crate::tracking::TrackedResource) -> Self {
        Self {
            extent: (source.width, source.height),
            format: transport_unorm_dxgi_format(source.format),
        }
    }
}

/// Per-swapchain capture state stored in the swapchain's hook context.
///
/// Device tracking is fixed at claim time; the ring behind the mutex is created
/// on demand and released across resizes.
pub struct SwapchainCapture {
    stream_id: u64,
    tracking: Arc<Tracking>,
    capture: Mutex<Vec<Capture>>,
    history: Mutex<polymer_presentation::history::ResourceHistory<crate::tracking::Presentation>>,
}

/// Claims a DXGI swapchain created for a D3D11 device, returning the
/// per-swapchain owner the central hook drives from its trampolines.
///
/// # Safety
///
/// `swapchain` and `creation_device` must be null or point to live COM
/// interfaces.
pub unsafe fn claim(
    swapchain: *mut c_void,
    creation_device: *mut c_void,
    tracking: Arc<Tracking>,
) -> Option<SwapchainOwnerHandle> {
    if swapchain.is_null() || creation_device.is_null() {
        return None;
    }
    let is_d3d11 = unsafe { IUnknown::from_raw_borrowed(&creation_device) }
        .is_some_and(|unknown| unknown.cast::<ID3D11Device>().is_ok());
    if !is_d3d11 {
        return None;
    }
    unsafe { tracking.register_swapchain(swapchain) };
    Some(SwapchainOwnerHandle::new(SwapchainCapture {
        stream_id: swapchain as u64,
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
        unsafe { self.tracking.register_swapchain(swapchain) };
        let requested =
            polymer_hook::config::tracked_render_target_indices().unwrap_or_else(|| vec![0]);
        let limit = requested.iter().copied().max().map_or(1, |index| index + 1);
        let presentations = if polymer_hook::config::tracking_enabled() {
            self.tracking.take_swapchain_presentations(swapchain, limit)
        } else {
            Vec::new()
        };
        let exact = !presentations.is_empty()
            && presentations
                .iter()
                .all(|presentation| presentation.selection.exact);
        let reconciled = self
            .history
            .lock()
            .reconcile(presentations, exact, |presentation| {
                presentation.revalidate()
            });
        let presentations = &reconciled.surfaces;
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
        use polymer_presentation::registry::SourceIdentity;
        polymer_presentation::producer::ProducerWindows::refresh(&self.tracking.registry);
        let sources = reconciled.registry_sources(|presentation| {
            let (resource, subresource) = presentation.identity();
            (
                SourceIdentity {
                    domain: 0,
                    resource: if resource == 0 {
                        self.stream_id
                    } else {
                        resource as u64
                    },
                    subresource,
                },
                presentation.selection.source_rect.extent,
            )
        });
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
        for (slot, capture) in captures.iter_mut().enumerate() {
            if !resolved.iter().any(|(selected, _)| *selected == slot) {
                capture.park();
            }
        }
        for (slot, index) in resolved {
            if reconciled.changed(index) {
                captures[slot].source_changed();
            }
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

    /// Releases the capture ring before the swapchain buffers resize.
    ///
    /// # Safety
    ///
    /// `swapchain` must point to the live swapchain being resized.
    unsafe fn before_resize(&self, swapchain: *mut c_void) {
        self.history.lock().reset();
        unsafe { self.tracking.release_swapchain(swapchain) };
        let previous = std::mem::take(&mut *self.capture.lock());
        if previous.iter().any(|capture| capture.native.is_some()) {
            tracing::debug!("polymer: released D3D11 capture for ResizeBuffers");
        }
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
        let tracked_source = presentation.and_then(|value| {
            let mut source = value.source.clone()?;
            let crop = value.selection.source_rect;
            if source.sample_count != 1
                && crop
                    != (polymer_presentation::Extent2D {
                        width: source.width,
                        height: source.height,
                    })
                    .rect()
            {
                return None;
            }
            source.x = crop.x.max(0) as u32;
            source.y = crop.y.max(0) as u32;
            source.width = crop.extent.width;
            source.height = crop.extent.height;
            Some(source)
        });
        let tracked_source = tracked_source.as_ref();
        let tracked_region = tracked_source
            .is_none()
            .then(|| {
                presentation.map(|value| {
                    value
                        .selection
                        .presentation_rect
                        .unwrap_or(value.selection.target_extent.rect())
                        .into()
                })
            })
            .flatten();
        let requirements = (
            requested_index,
            tracked_source.map(SourceRequirements::from_source),
        );
        if capture.requirements.as_ref() != Some(&requirements) {
            capture.invalidate();
            capture.requirements = Some(requirements);
        }
        if capture
            .native
            .as_ref()
            .is_some_and(|native| !native.connected())
        {
            capture.park();
        }
        if capture.native.is_none() && capture.retry.begin() {
            match unsafe { CaptureState::new(swapchain, tracked_source, slot) } {
                Ok(Some(native)) => capture.native = Some(native),
                Ok(None) => capture.park(),
                Err(error) => tracing::debug!("polymer: capture initialization deferred: {error}"),
            }
        }
        if let Some(native) = capture.native.as_mut() {
            if let Err(error) = unsafe {
                native.copy_frame(
                    presentation,
                    tracked_source,
                    tracked_region,
                    StreamId(self.stream_id ^ ((slot as u64) << 48)),
                )
            } {
                tracing::debug!("polymer: capture failed: {error}");
            }
        }
    }
}

fn send_stream_ready(
    connection: &mut ProducerSession,
    capture: &D3d11SwapchainCapture,
    slot: usize,
) -> std::io::Result<()> {
    let viewer_pid = connection.viewer_pid();
    let ring = capture.ring();

    let image_handles = ring
        .slots()
        .iter()
        .map(|slot| slot.raw_handle() as isize as i64)
        .collect::<Vec<_>>();
    let fence = match ring.raw_fence_handle() {
        Some(raw) => (raw as isize) as i64,
        None => 0,
    };
    let (image_kind, sync_kind, adapter_luid, consumer_fence) = match ring.mode() {
        D3d11RingMode::Fence => {
            let raw_consumer_fence = ring.raw_consumer_fence_handle().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "fence-mode D3D11 ring is missing its consumer fence",
                )
            })?;
            (
                ExternalImageKind::D3d11D3d12Resource,
                // The shared D3D11 fences open as ID3D12Fence on the consumer.
                ExternalSyncKind::D3d12Fence,
                capture.adapter_luid(),
                (raw_consumer_fence as isize) as i64,
            )
        }
        // The D3D12 consumer acquires the keyed mutexes through D3D11On12.
        D3d11RingMode::KeyedMutexNoFence => (
            ExternalImageKind::D3d11SharedResourceNt,
            ExternalSyncKind::D3d11KeyedMutex,
            capture.adapter_luid(),
            0,
        ),
        D3d11RingMode::KeyedMutex => (
            ExternalImageKind::D3d11SharedResourceNt,
            ExternalSyncKind::D3d11Fence,
            AdapterLuid {
                low_part: 0,
                high_part: 0,
            },
            0,
        ),
    };
    let desc = ring.desc();
    connection.publish(ExportDescriptor {
        surface_id: SurfaceId(if polymer_hook::config::tracking_enabled() {
            polymer_presentation::registry::PresentationRegistry::logical_surface_id(slot)
        } else {
            capture.source_window() as u64
        }),
        adapter_luid,
        source_window: capture.source_window() as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind,
        sync_kind,
        image_handles: &image_handles,
        timeline_semaphore_handle: fence,
        consumer_semaphore_handle: consumer_fence,
        bottom_up: false,
    })?;
    tracing::debug!("polymer: connected D3D11 ring to viewer process {viewer_pid}");
    Ok(())
}

impl Drop for SwapchainCapture {
    fn drop(&mut self) {
        self.tracking.registry.forget_owner(self.stream_id);
    }
}

impl CaptureState {
    fn connected(&self) -> bool {
        self.ipc.as_ref().is_some_and(ProducerSession::connected)
    }
}

impl CaptureState {
    unsafe fn new(
        swapchain: *mut c_void,
        tracked_source: Option<&crate::tracking::TrackedResource>,
        slot: usize,
    ) -> Result<Option<Self>, String> {
        let Some(mut connection) = ProducerSession::setup_result((|| {
            let session = polymer_hook::config::session()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
            ProducerSession::connect(&session)
        })())
        .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };

        let mode_chain: &[D3d11RingMode] = if connection.preferred_consumer() == ConsumerApi::D3d12
        {
            if polymer_hook::config::force_keyed_mutex() {
                &[D3d11RingMode::KeyedMutexNoFence, D3d11RingMode::KeyedMutex]
            } else {
                &[
                    D3d11RingMode::Fence,
                    D3d11RingMode::KeyedMutexNoFence,
                    D3d11RingMode::KeyedMutex,
                ]
            }
        } else {
            &[D3d11RingMode::KeyedMutex]
        };
        let mut ring = None;
        for (index, &mode) in mode_chain.iter().enumerate() {
            match unsafe { D3d11SwapchainCapture::new(swapchain, 3, mode, tracked_source) } {
                Ok(value) => {
                    ring = Some(value);
                    break;
                }
                Err(error) if index + 1 < mode_chain.len() => {
                    tracing::debug!(
                        "polymer: D3D11 {mode:?} capture unavailable, trying {:?}: {error}",
                        mode_chain[index + 1]
                    );
                }
                Err(error) => {
                    tracing::debug!("polymer: D3D11 capture creation failed: {error}");
                }
            }
        }
        let Some(ring) = ring else {
            return Err("capture initialization failed".into());
        };
        if ProducerSession::setup_result(send_stream_ready(&mut connection, &ring, slot))
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Ok(None);
        }
        let ipc = Some(connection);
        let desc = ring.ring().desc();
        tracing::debug!(
            "polymer: attached D3D11 {}x{} {}-slot shared texture ring",
            desc.width,
            desc.height,
            desc.slots
        );
        Ok(Some(CaptureState { capture: ring, ipc }))
    }
}

impl CaptureState {
    unsafe fn copy_frame(
        &mut self,
        presentation: Option<&crate::tracking::Presentation>,
        tracked_source: Option<&crate::tracking::TrackedResource>,
        tracked_region: Option<crate::D3d11CaptureRegion>,
        stream_id: StreamId,
    ) -> Result<Option<()>, windows::core::Error> {
        let submission = match unsafe { self.capture.copy_frame(tracked_region, tracked_source) } {
            Ok(submission) => submission,
            Err(error) if error.code() == windows::core::HRESULT(0x8007_0102u32 as i32) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };

        let capture_desc = self.capture.ring().desc();
        let placement =
            if let Some(presentation) = presentation.filter(|_| tracked_source.is_some()) {
                presentation.selection.frame_placement()
            } else {
                polymer_presentation::FramePlacement::new(
                    presentation.map_or(
                        polymer_presentation::Extent2D {
                            width: capture_desc.width,
                            height: capture_desc.height,
                        },
                        |value| value.selection.target_extent,
                    ),
                    Some(polymer_presentation::Rect {
                        x: submission.capture_x as i32,
                        y: submission.capture_y as i32,
                        extent: polymer_presentation::Extent2D {
                            width: submission.width,
                            height: submission.height,
                        },
                    }),
                    (false, false),
                )
            };
        let message = polymer_presentation::CapturedFrame {
            stream_id,
            slot: submission.slot,
            sync_value: submission.sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
            extent: polymer_presentation::Extent2D {
                width: submission.width,
                height: submission.height,
            },
        }
        .present(placement);
        ProducerSession::publish_frame(&mut self.ipc, message);
        Ok(Some(()))
    }
}
