use super::*;
use windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATE_PRESENT;

#[test]
fn clear_records_explicit_rectangles_at_execution_without_draw_bindings() {
    let tracking = Tracking::default();
    let handle = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 80 };
    let target = TrackedTarget {
        key: (100, 3),
        width: 32,
        height: 24,
    };
    tracking.rtv_descriptors.insert(handle.ptr, vec![target]);
    let list = std::ptr::dangling_mut::<c_void>();
    let rect = RECT {
        left: -4,
        top: 2,
        right: 16,
        bottom: 30,
    };
    unsafe { tracking.record_clear(list, handle, 1, &rect) };
    assert!(
        tracking
            .observations
            .graph(0)
            .observation_list()
            .0
            .is_empty()
    );
    // RTV descriptors are consumed at recording time, unlike sampled tables.
    tracking.rtv_descriptors.remove(&handle.ptr);
    unsafe { tracking.execute(7, 1, &list, || {}) };
    let (targets, edges) = tracking.observations.graph(0).observation_list();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, target.key);
    assert_eq!(targets[0].score, 0);
    assert_eq!(edges[0].coverage.unwrap().extent.area(), 16 * 22);
    assert_eq!(edges[0].source, target.key);
    assert_eq!(edges[0].destination, target.key);
    assert!(edges[0].mapping.is_none());
    unsafe { tracking.execute(7, 1, &list, || {}) };
    assert!(tracking.observations.graph(0).observation_list().1[0].order > edges[0].order);
    tracking.command_reset(list);
    tracking.observations.clear();
    unsafe { tracking.execute(7, 1, &list, || {}) };
    assert!(
        tracking
            .observations
            .graph(0)
            .observation_list()
            .0
            .is_empty()
    );
}

#[test]
fn submitted_batch_waits_for_presentation_clear_and_enters_the_next_generation() {
    let tracking = Tracking::default();
    let target = TrackedTarget {
        key: (100, 0),
        width: 640,
        height: 480,
    };
    let mut command = CommandState::default();
    Tracking::draw_target(
        &mut command,
        target,
        D3d12CaptureRegion {
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        },
        None,
    );
    tracking.commands.insert(1, command);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let native_submitted = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let boundary = tracking.observations.transaction();
        scope.spawn(|| {
            started_tx.send(()).unwrap();
            let lists = [std::ptr::dangling_mut::<c_void>()];
            unsafe {
                tracking.execute(7, 1, lists.as_ptr(), || {
                    assert!(
                        tracking
                            .observations
                            .graph(1)
                            .observation_list()
                            .0
                            .is_empty()
                    );
                    native_submitted.store(true, Ordering::SeqCst);
                })
            };
            finished_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            finished_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        tracking.observations.clear();
        assert!(!native_submitted.load(Ordering::SeqCst));
        tracking.resources.clear();
        tracking.observations.present(true);
        drop(boundary);
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
    });
    let (targets, edges) = tracking.observations.graph(1).observation_list();
    assert!(native_submitted.load(Ordering::SeqCst));
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, target.key);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].generation, 1);
    let lists = [std::ptr::dangling_mut::<c_void>()];
    unsafe { tracking.execute(7, 1, lists.as_ptr(), || {}) };
    let (replayed_targets, replayed_edges) = tracking.observations.graph(1).observation_list();
    assert_eq!(replayed_edges.len(), 1);
    assert_eq!(replayed_edges[0].generation, 1);
    assert!(replayed_edges[0].order > edges[0].order);
    assert_eq!(replayed_targets[0].order, targets[0].order);
}

#[test]
fn commands_are_invisible_until_submission_and_reset_discards_unsubmitted_work() {
    let tracking = Tracking::default();
    tracking.commands.entry(1).or_default().transitions.push((
        (42, u32::MAX),
        Some(D3d12CaptureSourceState::Legacy(
            D3D12_RESOURCE_STATE_PRESENT,
        )),
    ));
    assert!(!tracking.resource_states.contains_key(&(42, u32::MAX)));
    tracking.commit_list(1, 7);
    assert_eq!(*tracking.resource_queues.get(&(42, u32::MAX)).unwrap(), 7);
    tracking.commit_list(1, 8);
    assert_eq!(*tracking.resource_queues.get(&(42, u32::MAX)).unwrap(), 8);
    tracking.commands.entry(2).or_default().transitions.push((
        (43, u32::MAX),
        Some(D3d12CaptureSourceState::Legacy(
            D3D12_RESOURCE_STATE_PRESENT,
        )),
    ));
    tracking.command_reset(2usize as *mut c_void);
    tracking.commit_list(2, 7);
    assert!(!tracking.resource_states.contains_key(&(43, u32::MAX)));
}

#[test]
fn changing_root_layout_or_heaps_invalidates_descriptor_tables() {
    let tracking = Tracking::default();
    let list = std::ptr::dangling_mut::<c_void>();
    tracking.record_root_signature(list, 2usize as *mut c_void);
    tracking.record_root_descriptor_table(list, 0, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: 100 });
    tracking.record_root_signature(list, 2usize as *mut c_void);
    assert_eq!(tracking.commands.get(&1).unwrap().bindings.tables.len(), 1);
    tracking.record_root_signature(list, 3usize as *mut c_void);
    assert!(
        tracking
            .commands
            .get(&1)
            .unwrap()
            .bindings
            .tables
            .is_empty()
    );
    tracking.commands.get_mut(&1).unwrap().bindings.heaps = Some(vec![100]);
    tracking.record_root_descriptor_table(list, 0, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: 100 });
    unsafe {
        tracking.record_descriptor_heaps(list, 0, std::ptr::null());
    }
    assert!(
        tracking
            .commands
            .get(&1)
            .unwrap()
            .bindings
            .tables
            .is_empty()
    );
}

#[test]
fn bundle_draw_inherits_targets_and_bindings_only_when_executed() {
    let tracking = Tracking::default();
    let parent = std::ptr::dangling_mut::<c_void>();
    let bundle = 2usize as *mut c_void;
    {
        let mut state = tracking.commands.entry(1).or_default();
        state.viewport = Some(D3D12_VIEWPORT {
            Width: 640.0,
            Height: 480.0,
            ..Default::default()
        });
        state.render_targets.push(TrackedTarget {
            key: (100, 1),
            width: 640,
            height: 480,
        });
        state.bindings.signature = Some(7);
        state.bindings.tables.insert(0, 200);
    }
    tracking.record_draw(bundle);
    assert!(tracking.commands.get(&2).unwrap().observations.is_empty());
    tracking.execute_bundle(parent, bundle);
    {
        let state = tracking.commands.get(&1).unwrap();
        let bindings = state.draw_bindings();
        assert_eq!(state.observations[0].0, (100, 1));
        assert_eq!(bindings[0].signature, Some(7));
        assert_eq!(bindings[0].tables[&0], 200);
    }
    assert!(
        tracking
            .observations
            .graph(0)
            .observation_list()
            .1
            .is_empty()
    );
    tracking.commit_list(1, 3);
    let (_, edges) = tracking.observations.graph(0).observation_list();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].destination, (100, 1));
    assert_eq!(edges[0].source, (100, 1));
    assert_eq!(
        edges[0].coverage.unwrap().extent,
        Extent2D {
            width: 640,
            height: 480
        }
    );
    tracking.command_reset(parent);
    assert!(
        tracking
            .commands
            .get(&1)
            .is_none_or(|state| state.observations.is_empty())
    );
}

#[test]
fn scissor_is_used_without_intersecting_the_viewport() {
    assert_eq!(
        Tracking::region_from_scissor(RECT {
            left: 160,
            top: 90,
            right: 1760,
            bottom: 990,
        }),
        Some(D3d12CaptureRegion {
            x: 160,
            y: 90,
            width: 1600,
            height: 900,
        })
    );
}

#[test]
fn tracking_instances_do_not_share_command_state() {
    let first = Tracking::default();
    let second = Tracking::default();
    let viewport = D3D12_VIEWPORT {
        Width: 1280.0,
        Height: 720.0,
        ..Default::default()
    };

    unsafe { first.record_viewports(std::ptr::dangling_mut::<c_void>(), 1, &raw const viewport) };

    assert!(first.commands.contains_key(&1));
    assert!(!second.commands.contains_key(&1));
}

impl CommandState {
    pub(super) fn draw_bindings(&self) -> Vec<&Bindings> {
        self.observations
            .iter()
            .filter_map(|(_, candidate)| candidate.bindings.as_ref())
            .collect()
    }
}
