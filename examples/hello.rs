use anyhow::Result;

use wrestler::{Context, Runner, Task};

struct HelloTask;

impl Task for HelloTask {
    fn name(&self) -> &'static str {
        "Hello"
    }

    fn execute(&self, _context: &mut Context) -> Result<()> {
        println!("Hello Wrestler!");
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut context = Context::default();

    let mut runner = Runner::new();

    runner.add(HelloTask);

    runner.run(&mut context)
}
