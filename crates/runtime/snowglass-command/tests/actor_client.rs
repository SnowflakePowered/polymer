use std::{future::Future, pin::Pin, time::Duration};

use snowglass_command::command_handler;

snowglass_command::actor_client! {
    client AsyncCalculator {
        closed: "calculator closed".to_owned();
        queue_timeout:
            2,
            Duration::from_millis(10)
            => "calculator queue timed out".to_owned();
        response_timeout:
            Duration::from_millis(50)
            => "calculator response timed out".to_owned();

        #[args(Divide)]
        async fn divide(dividend: u32, divisor: u32) -> Result<u32, String>;

        #[args(Echo)]
        async fn echo(value: impl Into<String>) -> Result<String, String>;

        #[oneshot]
        #[args(Stop)]
        async fn stop();
    }
}

#[test]
fn bounded_actor_distinguishes_queue_and_response_timeouts() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (queue, _receiver) = tokio::sync::mpsc::channel(1);
            queue.send(AsyncCalculatorCommand::Stop {}).await.unwrap();
            let client = AsyncCalculator::new(queue);
            assert_eq!(
                client.divide(42, 6).await,
                Err("calculator queue timed out".to_owned())
            );

            let (queue, mut receiver) = tokio::sync::mpsc::channel(1);
            let client = AsyncCalculator::new(queue);
            let request = tokio::spawn(async move { client.divide(42, 6).await });
            let held_command = receiver.recv().await.unwrap();
            assert_eq!(
                request.await.unwrap(),
                Err("calculator response timed out".to_owned())
            );
            drop(held_command);
        });
}

struct CalculatorHandler;

#[command_handler]
impl AsyncCalculatorCommandHandler for CalculatorHandler {
    type Output<'handler>
        = Pin<Box<dyn Future<Output = bool> + Send + 'handler>>
    where
        Self: 'handler;

    async fn divide(&mut self, dividend: u32, divisor: u32) -> Result<u32, String> {
        tokio::task::yield_now().await;
        let divisor = (divisor != 0)
            .then_some(divisor)
            .ok_or_else(|| "division by zero".to_owned())?;
        Ok(dividend / divisor)
    }

    async fn echo(&mut self, value: String) -> Result<String, String> {
        Ok(value)
    }

    fn stop<'handler>(&'handler mut self) -> Self::Output<'handler> {
        Box::pin(async { true })
    }
}

#[test]
fn bounded_actor_dispatches_async_handlers_and_notifications() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (commands, mut receiver) = tokio::sync::mpsc::channel(1);
            let client = AsyncCalculator::new(commands);
            let worker = tokio::spawn(async move {
                let mut handler = CalculatorHandler;
                while let Some(command) = receiver.recv().await {
                    if command.dispatch(&mut handler).await {
                        break;
                    }
                }
            });

            assert_eq!(client.divide(42, 6).await, Ok(7));
            assert_eq!(
                client.divide(42, 0).await,
                Err("division by zero".to_owned())
            );
            let borrowed = "borrowed input".to_owned();
            assert_eq!(client.echo(borrowed.as_str()).await, Ok(borrowed));
            client.stop().await;
            worker.await.unwrap();
        });
}
