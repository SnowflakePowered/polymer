use crate::{Extent2D, configuration::RenderConfiguration, registry::PresentationRegistry};
use regex::Regex;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct WindowIdentity {
    pub handle: u64,
    pub lifetime: u64,
}

/// Canonical API/device/object identity plus a serial that is never reused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SourceIdentity {
    pub device: u64,
    pub object: u64,
    pub lifetime: u64,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub source: SourceIdentity,
    pub window: WindowIdentity,
    pub title: String,
}

/// Constructed only by a validated registry, immutable after allocation commits.
#[derive(Clone, Debug)]
pub struct FixedSurfaceBinding {
    pub(crate) ordinal: usize,
    pub(crate) configured_index: u32,
    pub(crate) logical_id: u64,
    pub(crate) extent: Extent2D,
    pub(crate) rotation: i32,
    pub(crate) source: SourceIdentity,
    pub(crate) window: WindowIdentity,
    pub(crate) generation: u64,
}

impl FixedSurfaceBinding {
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    pub fn configured_index(&self) -> u32 {
        self.configured_index
    }
    pub fn logical_id(&self) -> u64 {
        self.logical_id
    }
    pub fn extent(&self) -> Extent2D {
        self.extent
    }
    pub fn rotation(&self) -> i32 {
        self.rotation
    }
    pub fn source(&self) -> SourceIdentity {
        self.source
    }
    pub fn window(&self) -> WindowIdentity {
        self.window
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

struct Request {
    index: u32,
    pattern: Option<Regex>,
    extent: Extent2D,
    rotation: i32,
    previous_window: Option<WindowIdentity>,
    binding: Option<Arc<FixedSurfaceBinding>>,
    committed: bool,
}

struct Assignments {
    requests: Vec<Request>,
    candidates: Vec<Candidate>,
    generation: u64,
}

/// Instantiate once in the active producer, shared by all its devices.
/// Only CPU claim transactions hold this lock; allocation uses a provisional lease.
pub struct AssignmentRegistry {
    state: Mutex<Assignments>,
}

impl AssignmentRegistry {
    /// The source's eligibility lifetime is independent of GPU retirement.
    /// Allocation failures and final frontend destruction release the claim.
    pub fn register(
        self: &Arc<Self>,
        candidate: Candidate,
    ) -> Result<SourceRegistration, &'static str> {
        let source = candidate.source;
        let mut state = self.state.lock().unwrap();
        if state
            .candidates
            .iter()
            .any(|candidate| candidate.source == source)
        {
            return Err("source lifetime already registered");
        }
        state.candidates.push(candidate);
        Ok(SourceRegistration {
            registry: self.clone(),
            source,
        })
    }
    pub fn new(configuration: &RenderConfiguration) -> Result<Arc<Self>, String> {
        configuration.validate()?;
        let requests = configuration
            .surfaces
            .iter()
            .map(|request| {
                let [width, height] = request.resolution.expect("validated resolution");
                Ok(Request {
                    index: request.index,
                    pattern: request
                        .pattern
                        .as_deref()
                        .map(Regex::new)
                        .transpose()
                        .map_err(|error| error.to_string())?,
                    extent: Extent2D { width, height },
                    rotation: request.rotation,
                    previous_window: None,
                    binding: None,
                    committed: false,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Arc::new(Self {
            state: Mutex::new(Assignments {
                requests,
                candidates: Vec::new(),
                generation: 0,
            }),
        }))
    }

    /// Caller gathers captions/window lifetimes before entering this transaction.
    /// Updating a caption never changes an existing claim.
    pub fn observe(&self, observations: impl IntoIterator<Item = Candidate>) {
        let mut state = self.state.lock().unwrap();
        for observation in observations {
            if let Some(candidate) = state
                .candidates
                .iter_mut()
                .find(|candidate| candidate.source == observation.source)
            {
                *candidate = observation;
            } else {
                state.candidates.push(observation);
            }
        }
    }

    pub fn reserve(self: &Arc<Self>, source: SourceIdentity) -> Option<ProvisionalBinding> {
        let mut state = self.state.lock().unwrap();
        if state.requests.iter().any(|request| {
            request
                .binding
                .as_ref()
                .is_some_and(|binding| binding.source == source)
        }) {
            return None;
        }
        let candidate = state
            .candidates
            .iter()
            .find(|candidate| candidate.source == source)?
            .clone();
        let assignments = state.resolve();
        let ordinal = assignments
            .iter()
            .position(|assigned| *assigned == Some(source))?;
        state.generation = state.generation.checked_add(1)?;
        let generation = state.generation;
        let request = &mut state.requests[ordinal];
        let binding = Arc::new(FixedSurfaceBinding {
            ordinal,
            configured_index: request.index,
            logical_id: PresentationRegistry::logical_surface_id(request.index as usize),
            extent: request.extent,
            rotation: request.rotation,
            source,
            window: candidate.window,
            generation,
        });
        request.binding = Some(binding.clone());
        Some(ProvisionalBinding {
            registry: self.clone(),
            binding,
            committed: false,
        })
    }

    /// Ends source eligibility; GPU retirement is independent of this operation.
    pub fn forget(&self, source: SourceIdentity) {
        let mut state = self.state.lock().unwrap();
        state
            .candidates
            .retain(|candidate| candidate.source != source);
        for request in &mut state.requests {
            if request
                .binding
                .as_ref()
                .is_some_and(|binding| binding.source == source)
            {
                if request.committed {
                    request.previous_window =
                        request.binding.as_ref().map(|binding| binding.window);
                }
                request.binding = None;
                request.committed = false;
            }
        }
    }
}

pub struct SourceRegistration {
    registry: Arc<AssignmentRegistry>,
    source: SourceIdentity,
}
impl SourceRegistration {
    /// Ends eligibility before remaining frontend references finish retiring.
    /// Repeated release (including Drop) cannot affect a replacement source.
    pub fn release(&self) {
        self.registry.forget(self.source);
    }

    pub fn reserve(&self) -> Option<ProvisionalBinding> {
        self.registry.reserve(self.source)
    }
    /// A WSI surface outlives its swapchain generations. Old bindings retain
    /// their immutable identity while allocation of a replacement begins.
    pub fn next_generation(&self) -> Option<ProvisionalBinding> {
        {
            let mut state = self.registry.state.lock().unwrap();
            for request in &mut state.requests {
                if request
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.source == self.source)
                {
                    request.previous_window =
                        request.binding.as_ref().map(|binding| binding.window);
                    request.binding = None;
                    request.committed = false;
                }
            }
        }
        self.reserve()
    }
}
impl Drop for SourceRegistration {
    fn drop(&mut self) {
        self.release();
    }
}

impl Assignments {
    fn resolve(&self) -> Vec<Option<SourceIdentity>> {
        let mut assigned = self
            .requests
            .iter()
            .map(|request| request.binding.as_ref().map(|binding| binding.source))
            .collect::<Vec<_>>();
        // Recreation associations have priority, then title-qualified requests,
        // then unqualified requests, always in requested display order.
        for phase in 0..3 {
            for (ordinal, request) in self.requests.iter().enumerate() {
                if assigned[ordinal].is_some() {
                    continue;
                }
                let candidate = self.candidates.iter().find(|candidate| {
                    !assigned.contains(&Some(candidate.source))
                        && request.eligible(candidate, phase)
                });
                if let Some(candidate) = candidate {
                    assigned[ordinal] = Some(candidate.source);
                }
            }
        }
        assigned
    }
}

impl Request {
    fn eligible(&self, candidate: &Candidate, phase: u8) -> bool {
        match phase {
            0 => self.previous_window == Some(candidate.window),
            1 => self
                .pattern
                .as_ref()
                .is_some_and(|pattern| pattern.is_match(&candidate.title)),
            _ => self.pattern.is_none(),
        }
    }
}

/// Dropping an allocation that has not committed releases only its CPU claim.
/// The allocating backend must separately unwind partial native allocations.
pub struct ProvisionalBinding {
    registry: Arc<AssignmentRegistry>,
    binding: Arc<FixedSurfaceBinding>,
    committed: bool,
}

impl ProvisionalBinding {
    pub fn shared_binding(&self) -> Arc<FixedSurfaceBinding> {
        self.binding.clone()
    }

    pub fn commit(mut self) -> Result<Arc<FixedSurfaceBinding>, &'static str> {
        let mut state = self.registry.state.lock().unwrap();
        let request = &mut state.requests[self.binding.ordinal];
        if !request
            .binding
            .as_ref()
            .is_some_and(|binding| Arc::ptr_eq(binding, &self.binding))
        {
            return Err("source destroyed during virtual allocation");
        }
        request.committed = true;
        self.committed = true;
        Ok(self.binding.clone())
    }
}

impl Drop for ProvisionalBinding {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self.registry.state.lock().unwrap();
        let request = &mut state.requests[self.binding.ordinal];
        if request
            .binding
            .as_ref()
            .is_some_and(|binding| Arc::ptr_eq(binding, &self.binding))
        {
            request.binding = None;
        }
    }
}
