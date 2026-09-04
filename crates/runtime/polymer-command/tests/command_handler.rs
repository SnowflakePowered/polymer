use polymer_command::command_handler;

polymer_command::command_client! {
    client Calculator {
        closed: "calculator closed".to_owned();

        #[args(Divide)]
        fn divide(dividend: u32, divisor: u32) -> Result<u32, String>;
    }
}

struct CalculatorHandler;

#[command_handler]
impl CalculatorCommandHandler for CalculatorHandler {
    type Output<'handler>
        = bool
    where
        Self: 'handler;

    fn divide(&mut self, dividend: u32, divisor: u32) -> Result<u32, String> {
        let divisor = (divisor != 0)
            .then_some(divisor)
            .ok_or_else(|| "division by zero".to_owned())?;
        Ok(dividend / divisor)
    }
}

#[test]
fn natural_control_flow_completes_a_command_response() {
    let (commands, receiver) = std::sync::mpsc::channel();
    let client = Calculator::new(commands);
    let worker = std::thread::spawn(move || {
        let mut handler = CalculatorHandler;
        for _ in 0..2 {
            let command = receiver.recv().unwrap();
            assert!(!command.dispatch(&mut handler));
        }
    });

    assert_eq!(client.divide(42, 6), Ok(7));
    assert_eq!(client.divide(42, 0), Err("division by zero".to_owned()));
    worker.join().unwrap();
}
