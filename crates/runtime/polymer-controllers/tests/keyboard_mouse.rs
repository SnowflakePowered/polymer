use consolespec::input::BindingKey;
use polymer_config::input::{ControllerBindings, Orientation, ProjectionMode};
use polymer_controllers::{
    layout::InputLayout,
    mapping::{HostSample, Mapping},
    model::Control,
};
use polymer_input::GamepadReport;

fn key(text: &str) -> BindingKey {
    text.parse().expect("valid binding key")
}

fn compile(layout: &str, overrides: &[(&str, &str)]) -> (&'static InputLayout, Mapping) {
    let layout = InputLayout::get(layout).expect("layout exists");
    let bindings = ControllerBindings::try_from_strings(
        overrides
            .iter()
            .map(|(target, source)| ((*target).to_owned(), (*source).to_owned())),
    )
    .expect("bindings parse");
    layout
        .validate_controller_bindings(&bindings)
        .expect("bindings validate");
    let mapping = Mapping::compile(
        layout,
        ProjectionMode::Position,
        Orientation::Vertical,
        &bindings,
    )
    .expect("mapping compiles");
    (layout, mapping)
}

fn press(mapping: &Mapping, codes: &[&str], mouse_buttons: u8) -> GamepadReport {
    let mut host = HostSample::default();
    for code in codes {
        host.keys.set(code, true);
    }
    host.mouse_buttons = mouse_buttons;
    mapping.apply(&GamepadReport::default(), &host)
}

fn button_mask(layout: &InputLayout, target: &str) -> u32 {
    match layout.projected_control(key(target)) {
        Some(Control::Button(index)) => 1 << index,
        other => panic!("{target} is not a button: {other:?}"),
    }
}

#[test]
fn key_override_replaces_the_default_keyboard_binding() {
    let (layout, mapping) = compile("SNES_CONTROLLER", &[("button.a", "key.KeyJ")]);
    let a = button_mask(layout, "button.a");
    assert_eq!(press(&mapping, &["KeyJ"], 0).buttons & a, a);
    assert_eq!(press(&mapping, &["KeyX"], 0).buttons & a, 0);
    let b = button_mask(layout, "button.b");
    assert_eq!(press(&mapping, &["KeyZ"], 0).buttons & b, b);
}

#[test]
fn disabled_target_ignores_its_default_key() {
    let (layout, mapping) = compile("SNES_CONTROLLER", &[("button.a", "none")]);
    let a = button_mask(layout, "button.a");
    assert_eq!(press(&mapping, &["KeyX"], 0).buttons & a, 0);
}

#[test]
fn keys_on_opposite_stick_halves_drive_one_axis() {
    let (_, mapping) = compile(
        "GAMECUBE_CONTROLLER",
        &[
            ("analog.stick.left.x+", "key.KeyL"),
            ("analog.stick.left.x-", "key.KeyH"),
        ],
    );
    assert_eq!(press(&mapping, &["KeyL"], 0).left_stick[0], i16::MAX);
    assert_eq!(press(&mapping, &["KeyH"], 0).left_stick[0], i16::MIN);
    assert_eq!(press(&mapping, &["KeyL", "KeyH"], 0).left_stick[0], 0);
}

#[test]
fn mouse_button_drives_a_button_target() {
    let (layout, mapping) = compile("SNES_CONTROLLER", &[("button.b", "mouse.button.0")]);
    let b = button_mask(layout, "button.b");
    assert_eq!(press(&mapping, &[], 1).buttons & b, b);
    assert_eq!(press(&mapping, &[], 0).buttons & b, 0);
}

#[test]
fn unusable_sources_are_rejected() {
    let layout = InputLayout::get("SNES_CONTROLLER").expect("layout exists");
    for (target, source) in [
        ("button.a", "key.NotAKey"),
        ("button.a", "mouse.delta.x"),
        ("button.a", "mouse.button.9"),
    ] {
        let bindings =
            ControllerBindings::try_from_strings([(target.to_owned(), source.to_owned())])
                .expect("bindings parse");
        assert!(
            layout.validate_controller_bindings(&bindings).is_err(),
            "{source} should not bind {target}"
        );
    }
}
