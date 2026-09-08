use crate::ExternalDevice;
use ash::vk::{self, Handle};
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const SUBMISSION_RING_SIZE: usize = 16;

/// Shared with the hook's queue entry points. No map guard crosses a driver call.
pub struct Queue {
    pub external: Arc<ExternalDevice>,
    pub handle: vk::Queue,
    pub family: u32,
    pub gate: parking_lot::ReentrantMutex<()>,
    pub alive: Arc<AtomicBool>,
    ring: Mutex<SubmissionRing>,
    completion: Arc<CompletionTimeline>,
    indeterminate: AtomicBool,
}

struct SubmissionResources {
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
}

struct SubmissionSlot {
    resources: SubmissionResources,
    completion: u64,
    submission: Option<Arc<Submission>>,
}

struct SubmissionRing {
    slots: Vec<SubmissionSlot>,
    cursor: usize,
    next_completion: u64,
}

struct CompletionTimeline {
    external: Arc<ExternalDevice>,
    alive: Arc<AtomicBool>,
    handle: AtomicU64,
}

impl CompletionTimeline {
    fn handle(&self) -> vk::Semaphore {
        use ash::vk::Handle;
        vk::Semaphore::from_raw(self.handle.load(Ordering::Acquire))
    }

    fn complete(&self, value: u64) -> Result<bool, vk::Result> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        unsafe {
            self.external
                .device
                .get_semaphore_counter_value(self.handle())
        }
        .map(|completed| completed >= value)
    }

    fn wait(&self, value: u64, timeout: u64) -> Result<(), vk::Result> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        let semaphores = [self.handle()];
        let values = [value];
        unsafe {
            self.external.device.wait_semaphores(
                &vk::SemaphoreWaitInfo::default()
                    .semaphores(&semaphores)
                    .values(&values),
                timeout,
            )
        }
    }

    fn destroy(&self) {
        use ash::vk::Handle;
        let raw = self.handle.swap(0, Ordering::AcqRel);
        if raw != 0 && self.alive.load(Ordering::Acquire) {
            unsafe {
                self.external
                    .device
                    .destroy_semaphore(vk::Semaphore::from_raw(raw), None);
            }
        }
    }
}
impl Drop for CompletionTimeline {
    fn drop(&mut self) {
        self.destroy();
    }
}

pub struct Semaphore {
    pub handle: vk::Semaphore,
    external: Arc<ExternalDevice>,
    alive: Arc<AtomicBool>,
}
impl Semaphore {
    pub fn new(queue: &Queue) -> Result<Arc<Self>, vk::Result> {
        let handle = unsafe {
            queue
                .external
                .device
                .create_semaphore(&Default::default(), None)?
        };
        Ok(Arc::new(Self {
            handle,
            external: queue.external.clone(),
            alive: queue.alive.clone(),
        }))
    }
}
impl Drop for Semaphore {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Acquire) {
            unsafe {
                self.external.device.destroy_semaphore(self.handle, None);
            }
        }
    }
}

/// Only Polymer's ownership handoff lives here. Guest commands are never stored.
pub struct Submission {
    completion: Arc<CompletionTimeline>,
    value: u64,
    retained: Vec<Arc<Semaphore>>,
    submitted: AtomicBool,
}
impl Submission {
    pub fn wait(&self, timeout: u64) -> Result<(), vk::Result> {
        self.completion.wait(self.value, timeout)
    }
    pub fn complete(&self) -> Result<bool, vk::Result> {
        self.completion.complete(self.value)
    }
}
impl Drop for Submission {
    fn drop(&mut self) {
        if !self.completion.alive.load(Ordering::Acquire) {
            return;
        }
        if self.submitted.load(Ordering::Acquire) && self.complete() != Ok(true) {
            // An indeterminate submission cannot release its dependencies.
            std::mem::forget(std::mem::take(&mut self.retained));
        }
    }
}

impl Queue {
    pub fn new(
        external: Arc<ExternalDevice>,
        handle: vk::Queue,
        family: u32,
        alive: Arc<AtomicBool>,
    ) -> Result<Arc<Self>, vk::Result> {
        let mut timeline = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let completion_handle = unsafe {
            external.device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut timeline),
                None,
            )?
        };
        let completion = Arc::new(CompletionTimeline {
            external: external.clone(),
            alive: alive.clone(),
            handle: AtomicU64::new(completion_handle.as_raw()),
        });
        let slots = (0..SUBMISSION_RING_SIZE)
            .map(|_| SubmissionSlot {
                resources: SubmissionResources {
                    pool: vk::CommandPool::null(),
                    command: vk::CommandBuffer::null(),
                },
                completion: 0,
                submission: None,
            })
            .collect();
        Ok(Arc::new(Self {
            external,
            handle,
            family,
            gate: parking_lot::ReentrantMutex::new(()),
            alive,
            ring: Mutex::new(SubmissionRing {
                slots,
                cursor: 0,
                next_completion: 1,
            }),
            completion,
            indeterminate: AtomicBool::new(false),
        }))
    }

    /// Binary values are zero; timeline values accompany their matching handles.
    pub fn submit(
        &self,
        barriers: &[vk::ImageMemoryBarrier<'_>],
        waits: &[(vk::Semaphore, u64)],
        signals: &[(vk::Semaphore, u64)],
        app_fence: vk::Fence,
        retained: Vec<Arc<Semaphore>>,
    ) -> Result<Arc<Submission>, vk::Result> {
        let _queue = self.gate.lock();
        if self.indeterminate.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        let device = &self.external.device;
        let mut ring = self.ring.lock();
        let completed = unsafe { device.get_semaphore_counter_value(self.completion.handle())? };
        let slot_index = (0..ring.slots.len())
            .map(|offset| (ring.cursor + offset) % ring.slots.len())
            .find(|&index| ring.slots[index].completion <= completed)
            .unwrap_or(ring.cursor);
        let required = ring.slots[slot_index].completion;
        if required > completed {
            self.completion.wait(required, u64::MAX)?;
        }
        ring.cursor = (slot_index + 1) % ring.slots.len();
        let completion_value = ring.next_completion;
        ring.next_completion = ring
            .next_completion
            .checked_add(1)
            .ok_or(vk::Result::ERROR_DEVICE_LOST)?;
        let slot = &mut ring.slots[slot_index];
        slot.submission = None;
        if slot.resources.pool != vk::CommandPool::null() {
            unsafe {
                device
                    .reset_command_pool(slot.resources.pool, vk::CommandPoolResetFlags::empty())?;
            }
        }
        let command = match self.handoff(barriers, &mut slot.resources) {
            Ok(command) => command,
            Err(error) => return Err(error),
        };
        let work = Arc::new(Submission {
            completion: self.completion.clone(),
            value: completion_value,
            retained,
            submitted: AtomicBool::new(false),
        });
        let wait_handles = waits.iter().map(|v| v.0).collect::<Vec<_>>();
        let wait_values = waits.iter().map(|v| v.1).collect::<Vec<_>>();
        let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
        let mut signal_handles = signals.iter().map(|v| v.0).collect::<Vec<_>>();
        signal_handles.push(self.completion.handle());
        let mut signal_values = signals.iter().map(|v| v.1).collect::<Vec<_>>();
        signal_values.push(completion_value);
        let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_values)
            .signal_semaphore_values(&signal_values);
        let commands = command.as_slice();
        let submit = vk::SubmitInfo::default()
            .push_next(&mut timeline)
            .wait_semaphores(&wait_handles)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&commands)
            .signal_semaphores(&signal_handles);
        if let Err(error) =
            unsafe { device.queue_submit(self.handle, &[submit], vk::Fence::null()) }
        {
            if matches!(
                error,
                vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
            ) {
                return Err(error);
            }
            work.submitted.store(true, Ordering::Release);
            slot.completion = completion_value;
            slot.submission = Some(work);
            self.indeterminate.store(true, Ordering::Release);
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        work.submitted.store(true, Ordering::Release);
        slot.completion = completion_value;
        slot.submission = Some(work.clone());
        // A separate empty submission signals the app's fence without lending
        // that resettable fence to our resource-lifetime bookkeeping.
        if app_fence != vk::Fence::null() {
            unsafe { device.queue_submit(self.handle, &[], app_fence) }
                .map_err(|_| vk::Result::ERROR_DEVICE_LOST)?;
        }
        Ok(work)
    }

    fn handoff(
        &self,
        barriers: &[vk::ImageMemoryBarrier<'_>],
        resources: &mut SubmissionResources,
    ) -> Result<Option<vk::CommandBuffer>, vk::Result> {
        if barriers.is_empty() {
            return Ok(None);
        }
        let device = &self.external.device;
        unsafe {
            if resources.pool == vk::CommandPool::null() {
                resources.pool = device.create_command_pool(
                    &vk::CommandPoolCreateInfo::default().queue_family_index(self.family),
                    None,
                )?;
                resources.command = device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(resources.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )?[0];
                if let Some(set_data) = self.external.set_loader_data {
                    use ash::vk::Handle;
                    let result = set_data(device.handle(), resources.command.as_raw() as *mut _);
                    if result != vk::Result::SUCCESS {
                        return Err(result);
                    }
                }
            }
            device.begin_command_buffer(
                resources.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_pipeline_barrier(
                resources.command,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                barriers,
            );
            device.end_command_buffer(resources.command)?;
            Ok(Some(resources.command))
        }
    }

    pub fn drain(&self) -> Result<(), vk::Result> {
        if self.indeterminate.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        let _queue = self.gate.lock();
        unsafe {
            self.external.device.queue_wait_idle(self.handle)?;
        }
        for slot in &mut self.ring.lock().slots {
            slot.submission = None;
            slot.completion = 0;
        }
        Ok(())
    }

    pub fn release_resources(&self) {
        let mut ring = self.ring.lock();
        for slot in &mut ring.slots {
            slot.submission = None;
            if slot.resources.pool != vk::CommandPool::null() {
                unsafe {
                    self.external
                        .device
                        .destroy_command_pool(slot.resources.pool, None);
                }
                slot.resources.pool = vk::CommandPool::null();
                slot.resources.command = vk::CommandBuffer::null();
            }
        }
        self.completion.destroy();
    }
}
