use super::*;

impl<B: AutomationBackend> Xa11yDriver<B> {
    pub(super) fn node(element: &ElementData) -> NodeSnapshot {
        NodeSnapshot {
            role: element.role.to_snake_case().to_owned(),
            name: element.name.clone(),
            value: element.value.clone(),
            description: element.description.clone(),
            bounds: element.bounds.map(|bounds| Bounds {
                x: f64::from(bounds.x),
                y: f64::from(bounds.y),
                width: f64::from(bounds.width),
                height: f64::from(bounds.height),
            }),
            actions: element.actions.clone(),
            states: serde_json::to_value(&element.states).unwrap_or(serde_json::Value::Null),
            numeric_value: element.numeric_value,
            min_value: element.min_value,
            max_value: element.max_value,
            stable_id: element.stable_id.clone(),
            process_id: element.pid,
            raw: serde_json::to_value(&element.raw).unwrap_or(serde_json::Value::Null),
            children: Vec::new(),
        }
    }

    pub(super) fn tree(
        &self,
        root: &ElementData,
        options: TreeOptions,
    ) -> Result<(NodeSnapshot, bool), AutomationError> {
        let options = options.validate()?;
        let mut remaining = options.max_nodes;
        let mut truncated = false;
        let root = self.tree_node(root, 0, options.max_depth, &mut remaining, &mut truncated)?;
        Ok((root, truncated))
    }

    pub(super) fn tree_node(
        &self,
        element: &ElementData,
        depth: usize,
        max_depth: usize,
        remaining: &mut usize,
        truncated: &mut bool,
    ) -> Result<NodeSnapshot, AutomationError> {
        *remaining = remaining.saturating_sub(1);
        let mut node = Self::node(element);
        if depth >= max_depth {
            if !self
                .provider
                .as_ref()
                .expect("tree roots require an attached provider")
                .get_children(Some(element))
                .map_err(Self::map_error)?
                .is_empty()
            {
                *truncated = true;
            }
            return Ok(node);
        }
        for child in self
            .provider
            .as_ref()
            .expect("tree roots require an attached provider")
            .get_children(Some(element))
            .map_err(Self::map_error)?
        {
            if *remaining == 0 {
                *truncated = true;
                break;
            }
            node.children.push(self.tree_node(
                &child,
                depth + 1,
                max_depth,
                remaining,
                truncated,
            )?);
        }
        Ok(node)
    }

    pub(super) fn perform(locator: &Locator, action: LocatorAction) -> Result<(), AutomationError> {
        let result = match action {
            LocatorAction::Press => locator.press(),
            LocatorAction::Focus => locator.focus(),
            LocatorAction::Blur => locator.blur(),
            LocatorAction::Toggle => locator.toggle(),
            LocatorAction::Select => locator.select(),
            LocatorAction::Expand => locator.expand(),
            LocatorAction::Collapse => locator.collapse(),
            LocatorAction::ShowMenu => locator.show_menu(),
            LocatorAction::Increment => locator.increment(),
            LocatorAction::Decrement => locator.decrement(),
            LocatorAction::ScrollIntoView => locator.scroll_into_view(),
            LocatorAction::SetValue(value) => locator.set_value(&value),
            LocatorAction::SetNumericValue(value) => locator.set_numeric_value(value),
            LocatorAction::TypeText(value) => locator.type_text(&value),
            LocatorAction::SetTextSelection { start, end } => locator.select_text(start, end),
            LocatorAction::Perform(action) => locator.perform_action(&action),
        };
        result.map_err(Self::map_error)
    }

    pub(super) fn wait(
        locator: &Locator,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<Option<NodeSnapshot>, AutomationError> {
        match condition {
            WaitCondition::Attached => locator
                .wait_attached(timeout)
                .map(|element| Some(Self::node(element.data()))),
            WaitCondition::Detached => locator.wait_detached(timeout).map(|()| None),
            WaitCondition::Visible => locator
                .wait_visible(timeout)
                .map(|element| Some(Self::node(element.data()))),
            WaitCondition::Hidden => locator.wait_hidden(timeout).map(|()| None),
            WaitCondition::Enabled => locator
                .wait_enabled(timeout)
                .map(|element| Some(Self::node(element.data()))),
            WaitCondition::Focused => locator
                .wait_focused(timeout)
                .map(|element| Some(Self::node(element.data()))),
        }
        .map_err(Self::map_error)
    }

    pub(super) fn event(event: xa11y_core::Event) -> UiEvent {
        let (event_type, state, value) = match event.kind {
            EventKind::FocusChanged => ("focusChanged", None, None),
            EventKind::ValueChanged => ("valueChanged", None, None),
            EventKind::NameChanged => ("nameChanged", None, None),
            EventKind::StateChanged { flag, value } => (
                "stateChanged",
                Some(Self::state_name(flag).to_owned()),
                Some(value),
            ),
            EventKind::StructureChanged => ("structureChanged", None, None),
            EventKind::WindowOpened => ("windowOpened", None, None),
            EventKind::WindowClosed => ("windowClosed", None, None),
            EventKind::WindowActivated => ("windowActivated", None, None),
            EventKind::WindowDeactivated => ("windowDeactivated", None, None),
            EventKind::SelectionChanged => ("selectionChanged", None, None),
            EventKind::MenuOpened => ("menuOpened", None, None),
            EventKind::MenuClosed => ("menuClosed", None, None),
            EventKind::TextChanged => ("textChanged", None, None),
            EventKind::Announcement => ("announcement", None, None),
            _ => ("unknown", None, None),
        };
        UiEvent {
            event_type: event_type.to_owned(),
            target: event.target.as_ref().map(Self::node),
            state,
            value,
        }
    }

    pub(super) fn state_name(flag: StateFlag) -> &'static str {
        match flag {
            StateFlag::Enabled => "enabled",
            StateFlag::Visible => "visible",
            StateFlag::Focused => "focused",
            StateFlag::Checked => "checked",
            StateFlag::Selected => "selected",
            StateFlag::Expanded => "expanded",
            StateFlag::Editable => "editable",
            StateFlag::Focusable => "focusable",
            StateFlag::Modal => "modal",
            StateFlag::Required => "required",
            StateFlag::Busy => "busy",
            _ => "unknown",
        }
    }

    pub fn map_error(error: Error) -> AutomationError {
        let (kind, diagnosis) = match &error {
            Error::SelectorNotMatched { diagnosis, .. } => (
                "selectorNotMatched",
                diagnosis.as_deref().map(Self::diagnosis).map(Box::new),
            ),
            Error::Timeout { diagnosis, .. } => (
                "timeout",
                diagnosis.as_deref().map(Self::diagnosis).map(Box::new),
            ),
            Error::ElementStale { .. } => ("staleElement", None),
            Error::ActionNotSupported { .. } | Error::TextValueNotSupported => {
                ("actionNotSupported", None)
            }
            Error::InvalidSelector { .. } | Error::InvalidActionData { .. } => {
                ("invalidArgument", None)
            }
            Error::PermissionDenied { .. } => ("permissionDenied", None),
            Error::Unsupported { .. } => ("unsupported", None),
            Error::Platform { .. } => ("platform", None),
            _ => ("accessibility", None),
        };
        AutomationError {
            kind: kind.to_owned(),
            message: error.to_string(),
            diagnosis,
        }
    }

    pub(super) fn diagnosis(value: &xa11y_core::Diagnosis) -> Diagnosis {
        Diagnosis {
            selector: value.selector.clone(),
            condition: value.condition.clone(),
            last_observed: value.last_observed.clone(),
            candidates: value.candidates.clone(),
            scope: value.scope.clone(),
        }
    }
}
