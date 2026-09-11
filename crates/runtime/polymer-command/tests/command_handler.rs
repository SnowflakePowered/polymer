use polymer_command::{Admission, Completion, ExecutorLifetime, LocalDelivery, command_handler};
use std::{sync::mpsc, thread, time::Duration};

polymer_command::command_client! {
    client Calculator {
        closed: "calculator closed".to_owned();
        #[args(Divide)]
        fn divide(dividend: u32, divisor: u32) -> Result<u32, String>;
        #[deferred]
        #[args(Hold)]
        fn hold(value: u32) -> Result<u32, String>;
        #[oneshot]
        #[args(Stop)]
        fn stop();
    }
}

struct CalculatorHandler {
    release: mpsc::Receiver<()>,
    started: mpsc::Sender<thread::ThreadId>,
    finished: mpsc::Sender<()>,
}

#[command_handler]
impl CalculatorCommandHandler for CalculatorHandler {
    type Output<'handler> = bool;
    fn divide(&mut self, dividend: u32, divisor: u32) -> Result<u32, String> {
        self.started.send(thread::current().id()).unwrap();
        dividend
            .checked_div(divisor)
            .ok_or_else(|| "division by zero".to_owned())
    }
    fn hold<'handler>(
        &'handler mut self,
        value: u32,
        response: Completion<Result<u32, String>>,
    ) -> Self::Output<'handler> {
        self.started.send(thread::current().id()).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        response.send(Ok(value));
        self.finished.send(()).unwrap();
        false
    }
    fn stop<'handler>(
        &'handler mut self,
        _completion: Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        true
    }
}

#[test]
fn local_work_holds_capacity_and_owner_drop_closes_with_clients_alive() {
    let (commands, receiver) = mpsc::channel::<CalculatorCommand>();
    let (release, releases) = mpsc::channel();
    let (started, starts) = mpsc::channel();
    let (finished, finishes) = mpsc::channel();
    let (cleaned, cleanup) = mpsc::channel();
    let lifetime = ExecutorLifetime::new(Admission::new(1), None);
    let client = Calculator::new(LocalDelivery::from(commands).with_lifetime(lifetime.clone()));
    let worker_lifetime = lifetime.clone();
    thread::spawn(move || {
        worker_lifetime.run(move || {
            let mut handler = CalculatorHandler {
                release: releases,
                started,
                finished,
            };
            while let Ok(command) = receiver.recv() {
                if command.dispatch(&mut handler) {
                    break;
                }
            }
            drop(handler);
            cleaned.send(thread::current().id()).unwrap();
        })
    });
    let stop = client.clone();
    let worker_owner =
        polymer_command::WorkerOwner::new(client.clone(), lifetime.clone(), move || stop.stop());
    assert_eq!(client.divide(42, 0), Err("division by zero".to_owned()));
    let owner = starts.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(owner, thread::current().id());
    let first = client.hold(1).unwrap();
    assert_eq!(starts.recv_timeout(Duration::from_secs(5)).unwrap(), owner);
    drop(first);
    assert!(client.hold(2).is_err());
    release.send(()).unwrap();
    finishes.recv_timeout(Duration::from_secs(5)).unwrap();
    let last = client.hold(3).unwrap();
    assert_eq!(starts.recv_timeout(Duration::from_secs(5)).unwrap(), owner);
    drop(worker_owner);
    assert!(!lifetime.is_running());
    release.send(()).unwrap();
    assert_eq!(last.blocking_recv().unwrap(), Ok(3));
    futures::executor::block_on(lifetime.wait_closed()).unwrap();
    assert_eq!(cleanup.recv_timeout(Duration::from_secs(5)).unwrap(), owner);
    assert!(client.hold(4).is_err());
}
