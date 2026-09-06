use ash::{vk, vk::Handle};
use dashmap::DashMap;
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq, Hash)]
struct PresentAttempt {
    queue: vk::Queue,
    presents: Vec<(vk::SwapchainKHR, u32)>,
    waits: Vec<vk::Semaphore>,
}

impl PresentAttempt {
    fn new(
        queue: vk::Queue,
        presents: &[(vk::SwapchainKHR, u32)],
        waits: &[vk::Semaphore],
    ) -> Self {
        let mut presents = presents.to_vec();
        presents.sort_unstable_by_key(|(swapchain, image)| (swapchain.as_raw(), *image));
        let mut waits = waits.to_vec();
        waits.sort_unstable_by_key(|wait| wait.as_raw());
        Self {
            queue,
            presents,
            waits,
        }
    }
}

/// A failed enqueue leaves the producer's replacement signal unconsumed, even
/// though its copy submission has already consumed the application's waits.
#[derive(Default)]
pub(super) struct PresentRetries {
    attempts: DashMap<PresentAttempt, PendingPresent>,
}

struct PendingPresent {
    live_presents: Vec<(vk::SwapchainKHR, u32)>,
    waits: Vec<vk::Semaphore>,
    // Keep native signals alive independently of capture stream replacement.
    semaphores: Vec<Arc<crate::swapchain::PresentationSemaphore>>,
}

/// Holds replacement signals through the downstream present even if another
/// swapchain's destruction removes the pending registry entry concurrently.
pub struct PresentationRetry {
    pub waits: Vec<vk::Semaphore>,
    pub(super) captured: Vec<(vk::SwapchainKHR, u32)>,
    pub(super) semaphores: Vec<Arc<crate::swapchain::PresentationSemaphore>>,
}

impl PresentationRetry {
    pub fn uncaptured(&self, presents: &[(vk::SwapchainKHR, u32)]) -> Vec<(vk::SwapchainKHR, u32)> {
        presents
            .iter()
            .copied()
            .filter(|present| !self.captured.contains(present))
            .collect()
    }
}

impl PresentRetries {
    fn matching_attempt(&self, requested: &PresentAttempt) -> Option<PresentAttempt> {
        if self
            .attempts
            .get(requested)
            .is_some_and(|pending| pending.live_presents == requested.presents)
        {
            return Some(requested.clone());
        }
        let mut candidates = self.attempts.iter().filter(|pending| {
            pending.key().queue == requested.queue
                && pending.key().waits == requested.waits
                && !requested.presents.is_empty()
                && requested
                    .presents
                    .iter()
                    .any(|present| pending.live_presents.contains(present))
        });
        let first = candidates.next()?.key().clone();
        candidates.next().is_none().then_some(first)
    }
    pub(super) fn forget_swapchain(&self, swapchain: vk::SwapchainKHR) {
        self.attempts.retain(|_, pending| {
            pending
                .live_presents
                .retain(|(owner, _)| *owner != swapchain);
            // Omitted owners may own the final replacement signal. Keep its
            // native references until the surviving batch enqueues or retires.
            !pending.live_presents.is_empty()
        });
    }
}

impl super::CaptureState {
    pub fn preserve_retry(
        &self,
        queue: vk::Queue,
        presents: &[(vk::SwapchainKHR, u32)],
        waits: &[vk::Semaphore],
        retry: PresentationRetry,
    ) {
        let requested = PresentAttempt::new(queue, presents, waits);
        let key = self
            .present_retries
            .matching_attempt(&requested)
            .unwrap_or(requested);
        let mut pending =
            self.present_retries
                .attempts
                .entry(key)
                .or_insert_with(|| PendingPresent {
                    live_presents: retry.captured,
                    waits: retry.waits,
                    semaphores: Vec::new(),
                });
        for semaphore in retry.semaphores {
            if !pending
                .semaphores
                .iter()
                .any(|existing| Arc::ptr_eq(existing, &semaphore))
            {
                pending.semaphores.push(semaphore);
            }
        }
    }
    pub fn retry_presentation(
        &self,
        queue: vk::Queue,
        presents: &[(vk::SwapchainKHR, u32)],
        waits: &[vk::Semaphore],
    ) -> Option<PresentationRetry> {
        let requested = PresentAttempt::new(queue, presents, waits);
        let key = self.present_retries.matching_attempt(&requested)?;
        self.present_retries
            .attempts
            .get(&key)
            .map(|attempt| PresentationRetry {
                waits: attempt.waits.clone(),
                captured: attempt.live_presents.clone(),
                semaphores: attempt.semaphores.clone(),
            })
    }

    pub(crate) fn presentation_result(
        &self,
        queue: vk::Queue,
        presents: &[(vk::SwapchainKHR, u32)],
        waits: &[vk::Semaphore],
        replacement: Option<&[vk::Semaphore]>,
        result: vk::Result,
    ) -> Vec<Arc<crate::swapchain::PresentationSemaphore>> {
        let requested = PresentAttempt::new(queue, presents, waits);
        let key = self
            .present_retries
            .matching_attempt(&requested)
            .unwrap_or(requested);
        if matches!(
            result,
            vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
        ) {
            if let Some(replacement) = replacement {
                let semaphores = presents
                    .iter()
                    .flat_map(|(swapchain, _)| {
                        self.swapchains
                            .get(swapchain)
                            .map(|state| state.presentation_semaphores.retain_signals(replacement))
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>();
                let mut pending =
                    self.present_retries
                        .attempts
                        .entry(key)
                        .or_insert_with(|| PendingPresent {
                            live_presents: Vec::new(),
                            waits: Vec::new(),
                            semaphores: Vec::new(),
                        });
                pending.waits = replacement.to_vec();
                for present in presents {
                    if !pending.live_presents.contains(present) {
                        pending.live_presents.push(*present);
                    }
                }
                for semaphore in semaphores {
                    if !pending
                        .semaphores
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, &semaphore))
                    {
                        pending.semaphores.push(semaphore);
                    }
                }
            }
            Vec::new()
        } else {
            self.present_retries
                .attempts
                .remove(&key)
                .map(|(_, pending)| pending.semaphores)
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    #[test]
    fn expanding_a_retry_captures_only_new_images_and_updates_its_final_signal() {
        let state = super::super::CaptureState::default();
        let queue = vk::Queue::from_raw(1);
        let first = [(vk::SwapchainKHR::from_raw(2), 0)];
        let expanded = [first[0], (vk::SwapchainKHR::from_raw(3), 1)];
        let original = [vk::Semaphore::from_raw(4)];
        let first_signal = [vk::Semaphore::from_raw(5)];
        let final_signal = [vk::Semaphore::from_raw(6)];
        state.presentation_result(
            queue,
            &first,
            &original,
            Some(&first_signal),
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        );
        let held = state
            .retry_presentation(queue, &expanded, &original)
            .unwrap();
        assert_eq!(held.waits, first_signal);
        assert_eq!(held.uncaptured(&expanded), expanded[1..]);
        state.finish_presentation(
            queue,
            &expanded,
            &original,
            Some(&final_signal),
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            None,
            Some(held),
        );
        let held = state
            .retry_presentation(queue, &expanded, &original)
            .unwrap();
        assert_eq!(held.waits, final_signal);
        assert!(held.uncaptured(&expanded).is_empty());
        assert_eq!(state.present_retries.attempts.len(), 1);
        state.presentation_result(
            queue,
            &expanded,
            &original,
            Some(&final_signal),
            vk::Result::SUCCESS,
        );
        assert!(state.retry_presentation(queue, &first, &original).is_none());
    }

    #[test]
    fn retries_keep_consumed_waits_scoped_to_the_original_batch_until_enqueue_or_destruction() {
        let state = super::super::CaptureState::default();
        let retry = |queue, presents: &[(vk::SwapchainKHR, u32)], waits: &[vk::Semaphore]| {
            state
                .retry_presentation(queue, presents, waits)
                .map(|retry| retry.waits)
        };
        let queue = vk::Queue::from_raw(1);
        let presents = [
            (vk::SwapchainKHR::from_raw(2), 0),
            (vk::SwapchainKHR::from_raw(3), 1),
        ];
        let original = [vk::Semaphore::from_raw(4), vk::Semaphore::from_raw(7)];
        let replacement = [vk::Semaphore::from_raw(5)];
        for error in [
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        ] {
            state.presentation_result(queue, &presents, &original, Some(&replacement), error);
            assert_eq!(
                retry(queue, &presents, &original),
                Some(replacement.to_vec())
            );
            assert_eq!(state.present_retries.attempts.len(), 1);
        }
        assert!(retry(vk::Queue::from_raw(6), &presents, &original).is_none());
        let reversed_presents = [presents[1], presents[0]];
        let reversed_waits = [original[1], original[0]];
        assert_eq!(
            retry(queue, &reversed_presents, &reversed_waits),
            Some(replacement.to_vec())
        );
        state.presentation_result(
            queue,
            &reversed_presents,
            &reversed_waits,
            Some(&replacement),
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        );
        assert_eq!(
            state.present_retries.attempts.len(),
            1,
            "reordering a failed batch created a second pending signal"
        );
        assert_eq!(
            retry(queue, &presents[..1], &original),
            Some(replacement.to_vec())
        );
        state.presentation_result(
            queue,
            &presents[..1],
            &original,
            Some(&replacement),
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        );
        assert_eq!(
            state.present_retries.attempts.len(),
            1,
            "subset failure lost the original pending batch"
        );
        assert!(retry(queue, &[(presents[0].0, 9)], &original).is_none());
        assert!(retry(queue, &presents, &replacement).is_none());
        state.presentation_result(
            queue,
            &reversed_presents,
            &reversed_waits,
            Some(&replacement),
            vk::Result::ERROR_OUT_OF_DATE_KHR,
        );
        assert!(retry(queue, &presents, &original).is_none());
        state.presentation_result(
            queue,
            &presents,
            &original,
            Some(&replacement),
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        );
        let held = state
            .retry_presentation(queue, &presents[..1], &original)
            .unwrap();
        state.present_retries.forget_swapchain(presents[1].0);
        assert_eq!(
            state
                .retry_presentation(queue, &presents, &original)
                .unwrap()
                .uncaptured(&presents),
            presents[1..]
        );
        assert_eq!(
            retry(queue, &presents[..1], &original),
            Some(replacement.to_vec()),
            "destroying one owner discarded the surviving retry"
        );
        assert!(
            retry(queue, &presents[1..], &original).is_none(),
            "a destroyed or reused handle inherited a pending image"
        );
        state.preserve_retry(queue, &presents[..1], &original, held);
        assert_eq!(
            retry(queue, &presents[..1], &original),
            Some(replacement.to_vec())
        );
        state.presentation_result(
            queue,
            &presents[..1],
            &original,
            Some(&replacement),
            vk::Result::SUCCESS,
        );
        assert!(state.present_retries.attempts.is_empty());
        state.presentation_result(
            queue,
            &presents,
            &original,
            Some(&replacement),
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        );
        state.present_retries.forget_swapchain(presents[1].0);
        state.present_retries.forget_swapchain(presents[0].0);
        assert!(state.present_retries.attempts.is_empty());
    }
}
