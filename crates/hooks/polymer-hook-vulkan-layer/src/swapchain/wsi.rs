use super::wakeup::AcquireWakeup;
use super::*;
use polymer_presentation::producer::{ProducerSession, ProducerWindows};
use polymer_producer_vulkan::swapchain::{AcquireInfo, CreateInfo, ReleaseTimeline};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) struct Live {
    pub controller: Option<SwapchainController<VulkanVirtualBackend>>,
    pub leases: Vec<Option<AppLease>>,
}
pub(super) struct Swapchain {
    pub device: Arc<Device>,
    pub surface: vk::SurfaceKHR,
    pub images: Vec<vk::Image>,
    pub live: OwnerCell<Live>,
    pub closed: AtomicBool,
    pub wakeup: AcquireWakeup,
    release: ReleaseTimeline,
    attachment: Attachment,
}

const ACQUIRE_WAIT_SLICE: Duration = Duration::from_millis(8);
/// Every virtual swapchain paces acquisition to one queued frame, woken by the
/// consumer's release timeline, so applications start frames just in time.
const MAXIMUM_LATENCY: u32 = 1;

enum Attachment {
    Bound {
        _window: polymer_platform_windows::FixedWindowBinding,
    },
    Detached(Arc<Surface>),
}

impl Swapchain {
    unsafe fn create(
        device: Arc<Device>,
        info: &vk::SwapchainCreateInfoKHR<'_>,
        surface: Arc<Surface>,
    ) -> Result<Arc<Self>, vk::Result> {
        if info.old_swapchain != vk::SwapchainKHR::null() {
            let old = Self::get(device.external.device.handle(), info.old_swapchain)?
                .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
            if old.surface != info.surface {
                return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
            }
            old.closed.store(true, Ordering::Release);
        }
        let extent = polymer_presentation::Extent2D {
            width: info.image_extent.width,
            height: info.image_extent.height,
        };
        let (controller, attachment) = match surface.binding() {
            Some(_) => unsafe { Self::bound(&device, info, &surface, extent)? },
            None => unsafe { Self::detached(&device, info, &surface, extent)? },
        };
        let images = (0..controller.profile().image_count)
            .map(|i| {
                controller
                    .api_access(ImageSlot::new(i, controller.profile().image_count).unwrap())
                    .map_err(WsiResult::controller)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let release = controller.backend().release_timeline();
        for &image in &images {
            device.images.insert(image, ());
        }
        let count = images.len();
        Ok(Arc::new(Self {
            device,
            surface: info.surface,
            images,
            live: OwnerCell::new(Live {
                controller: Some(controller),
                leases: (0..count).map(|_| None).collect(),
            }),
            closed: AtomicBool::new(false),
            wakeup: AcquireWakeup::default(),
            release,
            attachment,
        }))
    }
    unsafe fn bound(
        device: &Arc<Device>,
        info: &vk::SwapchainCreateInfoKHR<'_>,
        surface: &Surface,
        extent: polymer_presentation::Extent2D,
    ) -> Result<(SwapchainController<VulkanVirtualBackend>, Attachment), vk::Result> {
        let reservation = surface
            .generation(extent)
            .ok_or(vk::Result::ERROR_NATIVE_WINDOW_IN_USE_KHR)?;
        let binding = reservation.shared_binding();
        unsafe {
            Self::validate(device, info, binding.extent())?;
        }
        let window = ProducerWindows::bind_fixed(&binding)
            .map_err(|_| vk::Result::ERROR_SURFACE_LOST_KHR)?;
        let session = ProducerSession::connect(
            &polymer_hook::config::session()
                .map_err(|_| vk::Result::ERROR_INITIALIZATION_FAILED)?,
        )
        .map_err(|_| vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let controller =
            SwapchainController::create(Self::create_info(device, info), binding, session)
                .map_err(|failure| Self::failed(device, failure))?;
        let committed = reservation
            .commit()
            .map_err(|_| vk::Result::ERROR_SURFACE_LOST_KHR)?;
        surface.commit(committed);
        Ok((controller, Attachment::Bound { _window: window }))
    }
    unsafe fn detached(
        device: &Arc<Device>,
        info: &vk::SwapchainCreateInfoKHR<'_>,
        surface: &Arc<Surface>,
        extent: polymer_presentation::Extent2D,
    ) -> Result<(SwapchainController<VulkanVirtualBackend>, Attachment), vk::Result> {
        unsafe {
            Self::validate(device, info, extent)?;
        }
        let binding = surface
            .detached(extent)
            .ok_or(vk::Result::ERROR_SURFACE_LOST_KHR)?;
        let controller = SwapchainController::detached(
            Self::create_info(device, info),
            binding,
            polymer_protocol::ConsumerApi::D3d12,
        )
        .map_err(|failure| Self::failed(device, failure))?;
        Ok((controller, Attachment::Detached(surface.clone())))
    }
    fn create_info(device: &Device, info: &vk::SwapchainCreateInfoKHR<'_>) -> CreateInfo {
        CreateInfo {
            external: device.external.clone(),
            acquire_queue: device.acquire_queue.clone(),
            queue_families: device.families.clone(),
            format: info.image_format,
            usage: info.image_usage,
            count: info.min_image_count,
        }
    }
    fn failed(device: &Device, failure: CreationFailure<VulkanVirtualBackend>) -> vk::Result {
        if let Some(retirement) = failure.retirement {
            device.retired.lock().push(retirement);
        }
        WsiResult::controller(failure.error)
    }
    /// A detached generation is out of date once its surface is assigned.
    pub(super) fn superseded(&self) -> bool {
        matches!(&self.attachment, Attachment::Detached(surface) if surface.binding().is_some())
    }
    unsafe fn validate(
        device: &Device,
        info: &vk::SwapchainCreateInfoKHR<'_>,
        extent: polymer_presentation::Extent2D,
    ) -> Result<(), vk::Result> {
        if !info.flags.is_empty() {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }
        unsafe {
            Self::validate_chain(info)?;
        }
        if info.image_extent.width != extent.width || info.image_extent.height != extent.height {
            return Err(vk::Result::ERROR_OUT_OF_DATE_KHR);
        }
        let profile = Surface::profile(device.physical)?;
        if info.image_extent.width > profile.max_extent.width
            || info.image_extent.height > profile.max_extent.height
        {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        if !profile
            .formats
            .iter()
            .any(|f| f.format == info.image_format && f.color_space == info.image_color_space)
            || !profile.usage.contains(info.image_usage)
            || info.image_usage.is_empty()
        {
            return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
        }
        if !(2..=4).contains(&info.min_image_count)
            || info.image_array_layers != 1
            || info.pre_transform != vk::SurfaceTransformFlagsKHR::IDENTITY
            || info.composite_alpha != vk::CompositeAlphaFlagsKHR::OPAQUE
            || !matches!(
                info.present_mode,
                vk::PresentModeKHR::FIFO | vk::PresentModeKHR::IMMEDIATE
            )
        {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        if info.image_sharing_mode == vk::SharingMode::CONCURRENT {
            let families = unsafe {
                std::slice::from_raw_parts(
                    info.p_queue_family_indices,
                    info.queue_family_index_count as usize,
                )
            };
            if families.len() < 2 || families.iter().any(|f| !device.families.contains(f)) {
                return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
            }
        }
        Ok(())
    }
    unsafe fn validate_chain(info: &vk::SwapchainCreateInfoKHR<'_>) -> Result<(), vk::Result> {
        let mut current = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while let Some(base) = unsafe { current.as_ref() } {
            match base.s_type {
                vk::StructureType::DEVICE_GROUP_SWAPCHAIN_CREATE_INFO_KHR => {
                    let modes = unsafe {
                        (*current.cast::<vk::DeviceGroupSwapchainCreateInfoKHR<'_>>()).modes
                    };
                    if modes != vk::DeviceGroupPresentModeFlagsKHR::LOCAL {
                        return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
                    }
                }
                vk::StructureType::IMAGE_FORMAT_LIST_CREATE_INFO => {
                    let formats = unsafe { &*current.cast::<vk::ImageFormatListCreateInfo<'_>>() };
                    let values = if formats.view_format_count == 0 {
                        &[]
                    } else {
                        unsafe {
                            std::slice::from_raw_parts(
                                formats.p_view_formats,
                                formats.view_format_count as usize,
                            )
                        }
                    };
                    if values.iter().any(|format| *format != info.image_format) {
                        return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
                    }
                }
                _ => return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT),
            }
            current = base.p_next;
        }
        Ok(())
    }
    fn acquire(
        &self,
        timeout: u64,
        semaphore: vk::Semaphore,
        fence: vk::Fence,
    ) -> Result<u32, vk::Result> {
        let until = (timeout != u64::MAX)
            .then(|| Instant::now().checked_add(Duration::from_nanos(timeout)))
            .flatten();
        let info = AcquireInfo { semaphore, fence };
        loop {
            let observed = self.wakeup.epoch();
            if self.device.lost.load(Ordering::Acquire) {
                return Err(vk::Result::ERROR_DEVICE_LOST);
            }
            self.device.poll_retired();
            if self.closed.load(Ordering::Acquire) || self.superseded() {
                return Err(vk::Result::ERROR_OUT_OF_DATE_KHR);
            }
            let mut in_flight = None;
            let mut pending_release = None;
            if let Some(mut state) = self.live.enter() {
                let controller = state
                    .controller
                    .as_mut()
                    .ok_or(vk::Result::ERROR_OUT_OF_DATE_KHR)?;
                match controller.acquire_with_info(
                    None,
                    AcquireDeadline::Nonblocking,
                    Some(MAXIMUM_LATENCY),
                    &info,
                ) {
                    Ok(lease) => {
                        let index = lease.slot().index();
                        state.leases[index as usize] = Some(lease);
                        return Ok(index);
                    }
                    Err(ControllerError::WouldBlock) => {
                        in_flight = controller.oldest_in_flight().cloned();
                        pending_release = controller
                            .pacing_release_target(MAXIMUM_LATENCY)
                            .or_else(|| controller.pending_release());
                    }
                    Err(error) => {
                        let error = WsiResult::controller(error);
                        if error == vk::Result::ERROR_DEVICE_LOST {
                            self.device.lost.store(true, Ordering::Release);
                        }
                        return Err(error);
                    }
                }
            }
            if timeout == 0 {
                return Err(vk::Result::NOT_READY);
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                return Err(vk::Result::TIMEOUT);
            }
            let remaining = until.map(|until| until.saturating_duration_since(Instant::now()));
            let slice = remaining.map_or(ACQUIRE_WAIT_SLICE, |r| r.min(ACQUIRE_WAIT_SLICE));
            let slice_ns = u64::try_from(slice.as_nanos()).unwrap_or(u64::MAX);
            let waited = match (in_flight, pending_release) {
                (Some(submission), _) => submission.wait(slice_ns),
                (None, Some(value)) => self.release.wait(value, slice_ns),
                (None, None) => {
                    self.wakeup.wait(observed, slice);
                    Ok(())
                }
            };
            match waited {
                Ok(()) | Err(vk::Result::TIMEOUT) => {}
                Err(error) => {
                    if error == vk::Result::ERROR_DEVICE_LOST {
                        self.device.lost.store(true, Ordering::Release);
                    }
                    return Err(error);
                }
            }
        }
    }
    pub(super) fn retire(&self) {
        self.closed.store(true, Ordering::Release);
        self.wakeup.notify();
        if let Ok(mut state) = self
            .live
            .enter_until(AcquireDeadline::after(Duration::from_secs(2)))
        {
            if let Some(controller) = state.controller.take() {
                self.device
                    .retired
                    .lock()
                    .push(controller.retire(RetireReason::ApplicationDestroyed));
            }
        }
        for image in &self.images {
            self.device.images.remove(image);
        }
        self.device.poll_retired();
    }
}

pub(crate) unsafe extern "system" fn create(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    _allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::SwapchainKHR,
) -> vk::Result {
    let result = Surface::get(unsafe { (*info).surface })
        .ok_or(vk::Result::ERROR_SURFACE_LOST_KHR)
        .and_then(|surface| Device::get(device).map(|device| (device, surface)))
        .and_then(|(device, surface)| unsafe { Swapchain::create(device, &*info, surface) });
    match result {
        Ok(swapchain) => {
            let handle = vk::SwapchainKHR::from_raw(Arc::as_ptr(&swapchain) as u64);
            SWAPCHAINS.insert(handle, swapchain);
            unsafe {
                *output = handle;
            }
            vk::Result::SUCCESS
        }
        Err(error) => {
            tracing::error!(?error, "virtual Vulkan swapchain creation failed");
            error
        }
    }
}
pub(crate) unsafe extern "system" fn destroy(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    _allocator: *const vk::AllocationCallbacks<'_>,
) {
    match Swapchain::get(device, swapchain) {
        Ok(Some(value)) => {
            SWAPCHAINS.remove(&swapchain);
            value.retire();
        }
        Err(_) | Ok(None) => {}
    }
}
pub(crate) unsafe extern "system" fn images(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    count: *mut u32,
    output: *mut vk::Image,
) -> vk::Result {
    match Swapchain::get(device, swapchain) {
        Ok(Some(value)) => unsafe { WsiResult::enumerate(&value.images, count, output) },
        Err(error) => error,
        Ok(None) => vk::Result::ERROR_OUT_OF_DATE_KHR,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkAcquireNextImageKHR(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    timeout: u64,
    semaphore: vk::Semaphore,
    fence: vk::Fence,
    output: *mut u32,
) -> vk::Result {
    match Swapchain::get(device, swapchain) {
        Ok(Some(value)) => match value.acquire(timeout, semaphore, fence) {
            Ok(index) => {
                unsafe {
                    *output = index;
                }
                vk::Result::SUCCESS
            }
            Err(e) => e,
        },
        Err(error) => error,
        Ok(None) => vk::Result::ERROR_OUT_OF_DATE_KHR,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkAcquireNextImage2KHR(
    device: vk::Device,
    info: *const vk::AcquireNextImageInfoKHR<'_>,
    output: *mut u32,
) -> vk::Result {
    let info = unsafe { &*info };
    match Swapchain::get(device, info.swapchain) {
        Ok(Some(value)) => {
            if info.device_mask != 1 || !info.p_next.is_null() {
                return vk::Result::ERROR_FEATURE_NOT_PRESENT;
            }
            match value.acquire(info.timeout, info.semaphore, info.fence) {
                Ok(index) => {
                    unsafe {
                        *output = index;
                    }
                    vk::Result::SUCCESS
                }
                Err(e) => e,
            }
        }
        Err(error) => error,
        Ok(None) => vk::Result::ERROR_OUT_OF_DATE_KHR,
    }
}
