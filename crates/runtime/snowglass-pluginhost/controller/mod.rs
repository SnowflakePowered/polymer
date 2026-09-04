snowglass_command::command_client! {
    pub client ControllerSession {
        closed: "controller plugin session is closed".to_owned();

        #[args(AttachAutomation)]
        pub fn attach_automation(
            session: snowglass_automation::AutomationSession,
        ) -> Result<(), String>;

        #[args(AttachActions)]
        fn attach_action_sink(
            sink: crate::actions::ActionSink,
        ) -> Result<(), String>;

        #[deferred]
        #[args(Changed)]
        pub fn change(
            change: snowglass_controllers::ControllerTopologyChange,
        ) -> Result<snowglass_controllers::ControllerTopology, String>;

        #[deferred]
        #[args(PauseResume)]
        fn pause_resume_with_timeout(
            state: PauseState,
            timeout: std::time::Duration,
        ) -> Result<(), String>;

        #[deferred]
        #[args(StateAction)]
        fn dispatch_state_action(
            slot: u32,
            save: bool,
            timeout: std::time::Duration,
        ) -> Result<(), String>;

        #[deferred]
        #[args(Quit)]
        fn quit_with_timeout(
            timeout: std::time::Duration,
        ) -> Result<Option<Vec<u8>>, String>;

        #[oneshot]
        #[args(Stop)]
        fn stop();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PauseState {
    Pause,
    Resume,
}

impl PauseState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }
}

impl ControllerSession {
    pub fn pause_resume(
        &self,
        state: PauseState,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        self.pause_resume_with_timeout(state, timeout.unwrap_or(DEFAULT_TIMEOUT))
    }

    pub fn save_state(
        &self,
        slot: u32,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        self.state_action(slot, true, timeout)
    }

    pub fn resume_state(
        &self,
        slot: u32,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        self.state_action(slot, false, timeout)
    }

    fn state_action(
        &self,
        slot: u32,
        save: bool,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        self.dispatch_state_action(slot, save, timeout.unwrap_or(DEFAULT_TIMEOUT))
    }

    #[allow(clippy::type_complexity)]
    pub fn quit(
        &self,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<Option<Vec<u8>>, String>>, String> {
        const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        self.quit_with_timeout(timeout.unwrap_or(DEFAULT_TIMEOUT))
    }

    pub fn attach_actions(
        &self,
        sink: impl Fn(&crate::actions::ParsedHotkey) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<(), String> {
        self.attach_action_sink(std::sync::Arc::new(sink))
    }
}

impl Drop for ControllerSession {
    fn drop(&mut self) {
        if self.sender.is_unique() {
            self.stop();
        }
    }
}

deno_core::extension!(
    snowglass_controller,
    esm_entry_point = "ext:snowglass_controller/00_controller.js",
    esm = [dir "controller/js", "00_controller.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    snowglass_controller::init()
}
