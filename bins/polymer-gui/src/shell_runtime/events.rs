use blitz_traits::events::{BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, DomEventData};

fn modifier_init(modifiers: keyboard_types::Modifiers) -> serde_json::Value {
    serde_json::json!({
        "altKey": modifiers.contains(keyboard_types::Modifiers::ALT),
        "ctrlKey": modifiers.contains(keyboard_types::Modifiers::CONTROL),
        "shiftKey": modifiers.contains(keyboard_types::Modifiers::SHIFT),
        "metaKey": modifiers.contains(keyboard_types::Modifiers::META),
    })
}

fn pointer_init(event: &BlitzPointerEvent) -> serde_json::Value {
    let mut init = modifier_init(event.mods);
    if let Some(init) = init.as_object_mut() {
        init.insert("clientX".into(), event.client_x().into());
        init.insert("clientY".into(), event.client_y().into());
        init.insert("screenX".into(), event.screen_x().into());
        init.insert("screenY".into(), event.screen_y().into());
        init.insert("button".into(), (event.button as u8).into());
        init.insert("buttons".into(), event.buttons.bits().into());
        init.insert("isPrimary".into(), event.is_primary.into());
        init.insert("pressure".into(), event.details.pressure.into());
        let (pointer_id, pointer_type) = match event.id {
            BlitzPointerId::Mouse => (1, "mouse"),
            BlitzPointerId::Pen => (2, "pen"),
            BlitzPointerId::Finger(id) => (id.saturating_add(2), "touch"),
        };
        init.insert("pointerId".into(), pointer_id.into());
        init.insert("pointerType".into(), pointer_type.into());
    }
    init
}

pub(super) fn event_init(event: &DomEventData) -> (&'static str, serde_json::Value) {
    match event {
        DomEventData::PointerMove(value)
        | DomEventData::PointerDown(value)
        | DomEventData::PointerUp(value)
        | DomEventData::PointerCancel(value)
        | DomEventData::PointerEnter(value)
        | DomEventData::PointerLeave(value)
        | DomEventData::PointerOver(value)
        | DomEventData::PointerOut(value) => ("pointer", pointer_init(value)),
        DomEventData::MouseMove(value)
        | DomEventData::MouseDown(value)
        | DomEventData::MouseUp(value)
        | DomEventData::MouseEnter(value)
        | DomEventData::MouseLeave(value)
        | DomEventData::MouseOver(value)
        | DomEventData::MouseOut(value)
        | DomEventData::Click(value)
        | DomEventData::ContextMenu(value)
        | DomEventData::DoubleClick(value) => ("mouse", pointer_init(value)),
        DomEventData::Wheel(value) => {
            let (delta_x, delta_y, delta_mode) = match value.delta {
                BlitzWheelDelta::Lines(x, y) => (x, y, 1),
                BlitzWheelDelta::Pixels(x, y) => (x, y, 0),
            };
            let mut init = modifier_init(value.mods);
            if let Some(init) = init.as_object_mut() {
                init.insert("clientX".into(), value.client_x().into());
                init.insert("clientY".into(), value.client_y().into());
                init.insert("deltaX".into(), delta_x.into());
                init.insert("deltaY".into(), delta_y.into());
                init.insert("deltaMode".into(), delta_mode.into());
                init.insert("buttons".into(), value.buttons.bits().into());
            }
            ("wheel", init)
        }
        DomEventData::KeyPress(value)
        | DomEventData::KeyDown(value)
        | DomEventData::KeyUp(value) => {
            let mut init = modifier_init(value.modifiers);
            if let Some(init) = init.as_object_mut() {
                init.insert("key".into(), value.key.to_string().into());
                init.insert("code".into(), format!("{:?}", value.code).into());
                init.insert("repeat".into(), value.is_auto_repeating.into());
                init.insert("isComposing".into(), value.is_composing.into());
            }
            ("keyboard", init)
        }
        DomEventData::Input(value) => (
            "input",
            serde_json::json!({ "data": value.value, "inputType": "insertText" }),
        ),
        DomEventData::Ime(value) => {
            let data = match value {
                blitz_traits::events::BlitzImeEvent::Preedit(value, _)
                | blitz_traits::events::BlitzImeEvent::Commit(value) => value.as_str(),
                _ => "",
            };
            ("composition", serde_json::json!({ "data": data }))
        }
        DomEventData::Focus(_)
        | DomEventData::Blur(_)
        | DomEventData::FocusIn(_)
        | DomEventData::FocusOut(_) => ("focus", serde_json::json!({})),
        DomEventData::Scroll(value) => (
            "event",
            serde_json::json!({
                "scrollTop": value.scroll_top,
                "scrollLeft": value.scroll_left,
            }),
        ),
        DomEventData::TouchStart(value)
        | DomEventData::TouchMove(value)
        | DomEventData::TouchEnd(value)
        | DomEventData::TouchCancel(value) => ("pointer", pointer_init(value)),
        DomEventData::AppleStandardKeybinding(value) => {
            ("event", serde_json::json!({ "binding": value.as_str() }))
        }
    }
}
