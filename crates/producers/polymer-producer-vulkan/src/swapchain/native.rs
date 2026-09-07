use crate::ExternalDevice;
use ash::vk;
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Shared with the hook's queue entry points. No map guard crosses a driver call.
pub struct Queue {
    pub external: Arc<ExternalDevice>,
    pub handle: vk::Queue,
    pub family: u32,
    pub gate: parking_lot::ReentrantMutex<()>,
    pub alive: Arc<AtomicBool>,
    pending: Mutex<Vec<Arc<Submission>>>,
    indeterminate: AtomicBool,
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
    external: Arc<ExternalDevice>,
    alive: Arc<AtomicBool>,
    pool: vk::CommandPool,
    fence: vk::Fence,
    retained: Vec<Arc<Semaphore>>,
    submitted: AtomicBool,
}
impl Submission {
    pub fn wait(&self, timeout: u64) -> Result<(), vk::Result> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        unsafe {
            self.external
                .device
                .wait_for_fences(&[self.fence], true, timeout)
        }
    }
    pub fn complete(&self) -> Result<bool, vk::Result> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        unsafe { self.external.device.get_fence_status(self.fence) }
    }
}
impl Drop for Submission {
    fn drop(&mut self) {
        if !self.alive.load(Ordering::Acquire) {
            return;
        }
        if self.submitted.load(Ordering::Acquire) && self.complete() != Ok(true) {
            // An indeterminate submission cannot release its dependencies.
            std::mem::forget(std::mem::take(&mut self.retained));
            return;
        }
        unsafe {
            if self.pool != vk::CommandPool::null() {
                self.external.device.destroy_command_pool(self.pool, None);
            }
            self.external.device.destroy_fence(self.fence, None);
        }
    }
}

impl Queue {
    pub fn new(
        external: Arc<ExternalDevice>,
        handle: vk::Queue,
        family: u32,
        alive: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            external,
            handle,
            family,
            gate: parking_lot::ReentrantMutex::new(()),
            alive,
            pending: Mutex::new(Vec::new()),
            indeterminate: AtomicBool::new(false),
        })
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
        let mut pending = std::mem::take(&mut *self.pending.lock());
        pending.retain(|work| work.complete() != Ok(true));
        self.pending.lock().extend(pending);
        let device = &self.external.device;
        let fence = unsafe { device.create_fence(&Default::default(), None)? };
        let mut pool = vk::CommandPool::null();
        let commands = match self.handoff(barriers, &mut pool) {
            Ok(commands) => commands,
            Err(error) => {
                unsafe {
                    if pool != vk::CommandPool::null() {
                        device.destroy_command_pool(pool, None);
                    }
                    device.destroy_fence(fence, None);
                }
                return Err(error);
            }
        };
        let work = Arc::new(Submission {
            external: self.external.clone(),
            alive: self.alive.clone(),
            pool,
            fence,
            retained,
            submitted: AtomicBool::new(false),
        });
        let wait_handles = waits.iter().map(|v| v.0).collect::<Vec<_>>();
        let wait_values = waits.iter().map(|v| v.1).collect::<Vec<_>>();
        let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
        let signal_handles = signals.iter().map(|v| v.0).collect::<Vec<_>>();
        let signal_values = signals.iter().map(|v| v.1).collect::<Vec<_>>();
        let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_values)
            .signal_semaphore_values(&signal_values);
        let submit = vk::SubmitInfo::default()
            .push_next(&mut timeline)
            .wait_semaphores(&wait_handles)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&commands)
            .signal_semaphores(&signal_handles);
        if let Err(error) = unsafe { device.queue_submit(self.handle, &[submit], fence) } {
            if matches!(
                error,
                vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
            ) {
                return Err(error);
            }
            work.submitted.store(true, Ordering::Release);
            self.indeterminate.store(true, Ordering::Release);
            self.pending.lock().push(work);
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        work.submitted.store(true, Ordering::Release);
        self.pending.lock().push(work.clone());
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
        pool: &mut vk::CommandPool,
    ) -> Result<Vec<vk::CommandBuffer>, vk::Result> {
        if barriers.is_empty() {
            return Ok(Vec::new());
        }
        let device = &self.external.device;
        unsafe {
            *pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(self.family),
                None,
            )?;
            let commands = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(*pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?;
            if let Some(set_data) = self.external.set_loader_data {
                use ash::vk::Handle;
                let result = set_data(device.handle(), commands[0].as_raw() as *mut _);
                if result != vk::Result::SUCCESS {
                    return Err(result);
                }
            }
            device.begin_command_buffer(
                commands[0],
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_pipeline_barrier(
                commands[0],
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                barriers,
            );
            device.end_command_buffer(commands[0])?;
            Ok(commands)
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
        let completed = std::mem::take(&mut *self.pending.lock());
        drop(completed);
        Ok(())
    }
}
