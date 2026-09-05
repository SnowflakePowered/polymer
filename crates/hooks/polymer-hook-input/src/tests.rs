use super::*;
use polymer_controllers::topology::{InputTopology, PointerBinding, PortBinding, RemapMode};
use polymer_input::{GamepadButton, KeyCode};
use polymer_ipc::input::{InputMessage, WireHostKey};

#[test]
fn keyboard_port_maps_without_a_physical_device_and_disconnects_when_unassigned() {
    let (control, _receiver) = subscriber::ControlSender::new();
    let input = HookInput {
        state: state::InputState::default(),
        host: HostState::default(),
        topology: topology::TopologyState::default(),
        control,
        connected: AtomicBool::new(true),
        controls: Mutex::new(VecDeque::new()),
    };
    let mut topology = InputTopology {
        ports: vec![PortBinding {
            player: 0,
            keyboard_mouse: true,
            group: "controller".to_owned(),
            port: 0,
            layout: "GAMECUBE_CONTROLLER".to_owned(),
            mode: RemapMode::Position,
            bindings: Vec::new(),
            pointer: PointerBinding::default(),
        }],
        ..Default::default()
    };
    input.set_topology(topology.clone());
    assert!(input.raw(0).is_some());
    assert!(input.raw(1).is_none());
    assert_eq!(input.mapped(0).unwrap().buttons, 0);
    let mut key = WireHostKey {
        code: KeyCode::new("KeyX").wire(),
        pressed: true,
        target_window: 0,
        virtual_key: 0,
        scan_code: 0,
        repeat: false,
        extended: false,
        timestamp_ms: 0,
        sequence: 0,
    };
    input.receive(InputMessage::HostKey(key));
    assert_ne!(
        input.mapped(0).unwrap().buttons & GamepadButton::A.mask(),
        0
    );
    key.pressed = false;
    input.receive(InputMessage::HostKey(key));
    assert_eq!(input.mapped(0).unwrap().buttons, 0);
    topology.ports[0].keyboard_mouse = false;
    input.set_topology(topology);
    assert!(input.raw(0).is_none());
    assert!(input.mapped(0).is_none());
}
