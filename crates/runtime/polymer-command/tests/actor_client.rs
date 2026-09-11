use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use kameo::{
    Actor,
    actor::{ActorRef, Spawn},
};
use polymer_command::{__kameo as kameo, Admission, native_handler};
use tokio::sync::Notify;

polymer_command::actor_client! {
    client AsyncCalculator() {
        closed: "calculator closed".to_owned();
        queue_timeout: 2, Duration::from_millis(10) => "queue timed out".to_owned();
        response_timeout: Duration::from_millis(50) => "reply timed out".to_owned();
        #[args(Work)]
        async fn work(id: u32, gate: Option<Arc<Notify>>) -> Result<usize, String>;
    }
}

struct Calculator {
    executions: Arc<AtomicUsize>,
    started: Arc<Notify>,
}

impl Actor for Calculator {
    type Args = Self;
    type Error = std::convert::Infallible;
    async fn on_start(state: Self, _: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(state)
    }
}

#[native_handler(AsyncCalculator)]
impl Calculator {
    async fn work(&mut self, id: u32, gate: Option<Arc<Notify>>) -> Result<usize, String> {
        let execution = self.executions.fetch_add(1, Ordering::SeqCst) + 1;
        self.started.notify_one();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        if id == 0 {
            return Err("domain rejection".to_owned());
        }
        Ok(execution)
    }
}

#[test]
fn admission_and_reply_deadlines_do_not_replay_accepted_work() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let executions = Arc::new(AtomicUsize::new(0));
            let started = Arc::new(Notify::new());
            let gate = Arc::new(Notify::new());
            let actor = Calculator::spawn_with_mailbox(
                Calculator {
                    executions: executions.clone(),
                    started: started.clone(),
                },
                kameo::mailbox::unbounded(),
            );
            let client = AsyncCalculator::from_actor(actor.clone(), Admission::new(1));
            let mut first = client.work(1, Some(gate.clone()));
            assert!(futures::poll!(first.as_mut()).is_pending());
            started.notified().await;
            let mut queued = client.work(2, None);
            assert!(futures::poll!(queued.as_mut()).is_pending());
            assert_eq!(
                client.work(3, None).await,
                Err("queue timed out".to_owned())
            );
            assert_eq!(executions.load(Ordering::SeqCst), 1);
            tokio::time::advance(Duration::from_millis(40)).await;
            assert_eq!(first.await, Err("reply timed out".to_owned()));
            assert_eq!(queued.await, Err("reply timed out".to_owned()));
            gate.notify_one();
            assert_eq!(client.work(4, None).await, Ok(3));
            assert_eq!(
                client.work(0, None).await,
                Err("domain rejection".to_owned())
            );
            actor.stop_gracefully().await.unwrap();
        });
}
