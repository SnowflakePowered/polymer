use ash::vk;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A private presentation fence remains alive until its queue operation finishes.
pub struct PresentationFence {
    device: ash::Device,
    raw: Vec<vk::Fence>,
    submitted: AtomicBool,
    retained: parking_lot::Mutex<Vec<Arc<crate::swapchain::PresentationSemaphore>>>,
}

impl PresentationFence {
    pub(crate) fn retain_signals(
        &self,
        signals: Vec<Arc<crate::swapchain::PresentationSemaphore>>,
    ) {
        self.retained.lock().extend(signals);
    }
    pub(crate) fn complete(&self) -> bool {
        !self.submitted.load(Ordering::Acquire)
            || self.raw.iter().all(|fence| {
                matches!(
                    unsafe { self.device.get_fence_status(*fence) },
                    Ok(true) | Err(vk::Result::ERROR_DEVICE_LOST)
                )
            })
    }

    pub(crate) fn wait(&self) -> bool {
        if !self.complete() {
            let result = unsafe { self.device.wait_for_fences(&self.raw, true, u64::MAX) };
            if !matches!(result, Ok(()) | Err(vk::Result::ERROR_DEVICE_LOST)) {
                tracing::error!(?result, "polymer: presentation fence retirement failed");
            }
        }
        self.complete()
    }
}

impl Drop for PresentationFence {
    fn drop(&mut self) {
        self.wait();
        if self.complete() {
            for fence in &self.raw {
                unsafe { self.device.destroy_fence(*fence, None) };
            }
        } else {
            std::mem::forget(std::mem::take(self.retained.get_mut()));
        }
    }
}

/// The hook keeps this guard through the downstream present call. Borrowed
/// application fences are waited before returning, before the app can reset them.
pub struct PresentationCompletion {
    device: ash::Device,
    owned: Option<Arc<PresentationFence>>,
    borrowed: vk::Fence,
    fences: Vec<vk::Fence>,
}

impl PresentationCompletion {
    /// `info` and its chain must remain valid through the downstream present.
    pub unsafe fn prepare(
        device: &ash::Device,
        info: &vk::PresentInfoKHR<'_>,
    ) -> Result<Self, vk::Result> {
        let mut node = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while !node.is_null() {
            if unsafe { (*node).s_type } == vk::StructureType::SWAPCHAIN_PRESENT_FENCE_INFO_EXT {
                let existing = unsafe { &*node.cast::<vk::SwapchainPresentFenceInfoEXT<'_>>() };
                let fences = unsafe {
                    std::slice::from_raw_parts(existing.p_fences, existing.swapchain_count as usize)
                };
                let borrowed = fences
                    .iter()
                    .copied()
                    .find(|fence| *fence != vk::Fence::null())
                    .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
                return Ok(Self {
                    device: device.clone(),
                    owned: None,
                    borrowed,
                    fences: Vec::new(),
                });
            }
            node = unsafe { (*node).p_next };
        }
        let mut owned = PresentationFence {
            device: device.clone(),
            raw: Vec::new(),
            submitted: AtomicBool::new(false),
            retained: Default::default(),
        };
        for _ in 0..info.swapchain_count {
            owned
                .raw
                .push(unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }?);
        }
        let fences = owned.raw.clone();
        Ok(Self {
            device: device.clone(),
            owned: Some(Arc::new(owned)),
            borrowed: vk::Fence::null(),
            fences,
        })
    }

    pub fn fences(&self) -> &[vk::Fence] {
        &self.fences
    }

    pub fn submitted(
        self,
        result: vk::Result,
    ) -> Result<Option<Arc<PresentationFence>>, vk::Result> {
        // Allocation errors guarantee that no queue operations were enqueued.
        // Surface/out-of-date errors still enqueue the semaphore waits.
        if matches!(
            result,
            vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
        ) {
            return Ok(None);
        }
        if !matches!(
            result,
            vk::Result::SUCCESS
                | vk::Result::SUBOPTIMAL_KHR
                | vk::Result::ERROR_OUT_OF_DATE_KHR
                | vk::Result::ERROR_SURFACE_LOST_KHR
                | vk::Result::ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT
                | vk::Result::ERROR_DEVICE_LOST
        ) {
            // Unknown failure semantics cannot establish whether a fence was
            // enqueued. Neither wait forever nor free possibly live payloads.
            std::mem::forget(self);
            return Err(result);
        }
        if let Some(owned) = self.owned {
            owned.submitted.store(true, Ordering::Release);
            Ok(Some(owned))
        } else {
            let result = unsafe {
                self.device
                    .wait_for_fences(&[self.borrowed], true, u64::MAX)
            };
            if let Err(error) = result {
                tracing::error!(
                    ?error,
                    "polymer: application presentation fence wait failed"
                );
                if error != vk::Result::ERROR_DEVICE_LOST {
                    return Err(error);
                }
            }
            Ok(None)
        }
    }
}

impl super::CaptureState {
    pub fn finish_presentation(
        &self,
        queue: vk::Queue,
        presents: &[(vk::SwapchainKHR, u32)],
        waits: &[vk::Semaphore],
        replacement: Option<&[vk::Semaphore]>,
        result: vk::Result,
        completion: Option<PresentationCompletion>,
        retry: Option<super::PresentationRetry>,
    ) {
        let mut retained = self.presentation_result(queue, presents, waits, replacement, result);
        if let Some(retry) = retry {
            retained.extend(retry.semaphores);
        }
        if matches!(
            result,
            vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
        ) {
            if let Some(waits_for_retry) = replacement {
                self.preserve_retry(
                    queue,
                    presents,
                    waits,
                    super::PresentationRetry {
                        waits: waits_for_retry.to_vec(),
                        captured: presents.to_vec(),
                        semaphores: retained,
                    },
                );
            }
            // The completion was never enqueued, so dropping it is safe.
            return;
        }
        match completion.map(|completion| completion.submitted(result)) {
            Some(Ok(Some(completion))) => {
                completion.retain_signals(retained);
                self.presentation_submitted(presents, completion);
            }
            Some(Err(_)) => {
                std::mem::forget(retained);
                self.presentation_wait_failed(presents);
            }
            // A borrowed application fence has already been waited. Without
            // maintenance support, existing swapchain pools retain ownership.
            Some(Ok(None)) => {}
            None => {
                // Without a completion fence, detached retry ownership cannot
                // be released based on queue submission completion alone.
                for semaphore in &retained {
                    semaphore.retain_without_completion();
                }
            }
        }
    }
    pub fn presentation_wait_failed(&self, presents: &[(vk::SwapchainKHR, u32)]) {
        for (swapchain, _) in presents {
            if let Some(mut state) = self.swapchains.get_mut(swapchain) {
                // The app may reset its fence after returning. Retain native
                // payloads indefinitely when the completion proof was lost.
                std::mem::forget(std::mem::take(&mut state.presentation_semaphores));
            }
        }
    }
    pub fn presentation_submitted(
        &self,
        presents: &[(vk::SwapchainKHR, u32)],
        completion: Arc<PresentationFence>,
    ) {
        for (swapchain, _) in presents {
            if let Some(state) = self.swapchains.get(swapchain) {
                state
                    .presentation_semaphores
                    .record_completion(completion.clone());
            }
        }
    }
}
