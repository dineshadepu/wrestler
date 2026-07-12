use anyhow::Result;

use crate::{Context, Task};

pub struct Runner {
    tasks: Vec<Box<dyn Task>>,
}

impl Runner {
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    pub fn add<T>(&mut self, task: T)
    where
        T: Task + 'static,
    {
        self.tasks.push(Box::new(task));
    }

    pub fn run(&self, context: &mut Context) -> Result<()> {
        for task in &self.tasks {
            println!("--------------------------------");
            println!("Running task: {}", task.name());

            task.execute(context)?;

            println!("✓ Success");
            println!();
        }

        Ok(())
    }
}
