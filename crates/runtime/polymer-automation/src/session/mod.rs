use tokio::sync::mpsc;

use crate::{AutomationDriver, AutomationError, AutomationRequest, AutomationResponse};
use polymer_command::command_handler;

// Sending clients are independent of the process-scoped worker owner.
polymer_command::command_client! {
    pub client AutomationClient {
        closed: AutomationError::process_exited();

        #[args(Execute)]
        pub async fn execute(
            request: AutomationRequest,
        ) -> Result<AutomationResponse, AutomationError>;

        #[oneshot]
        #[args(Stop)]
        fn stop();
    }
}

pub type AutomationSession = polymer_command::WorkerOwner<AutomationClient>;

impl AutomationClient {
    pub fn spawn(mut driver: impl AutomationDriver) -> Result<AutomationSession, AutomationError> {
        let (commands, mut receiver) = mpsc::unbounded_channel::<AutomationClientCommand>();
        let lifetime = polymer_command::ExecutorLifetime::default();
        let worker_lifetime = lifetime.clone();
        std::thread::Builder::new()
            .name("polymer-ui-automation".to_owned())
            .spawn(move || {
                worker_lifetime.clone().run(move || {
                    while let Some(command) = receiver.blocking_recv() {
                        if !worker_lifetime.is_running() {
                            break;
                        }
                        if command.dispatch(&mut driver) {
                            break;
                        }
                    }
                    driver.shutdown();
                })
            })
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        let client = Self::new(
            polymer_command::LocalDelivery::from(commands).with_lifetime(lifetime.clone()),
        );
        let stop = client.clone();
        Ok(polymer_command::WorkerOwner::new(
            client,
            lifetime,
            move || stop.stop(),
        ))
    }
}

#[command_handler]
impl<Driver: AutomationDriver> AutomationClientCommandHandler for Driver {
    type Output<'handler>
        = bool
    where
        Self: 'handler;

    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError> {
        AutomationDriver::execute(self, request)
    }

    fn stop<'handler>(
        &'handler mut self,
        _completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        true
    }
}
