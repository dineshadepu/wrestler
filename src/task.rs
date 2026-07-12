use anyhow::Result;

use crate::Context;

/// A unit of work.
///
/// Every action performed by Wrestler is represented as a task.
pub trait Task {
    /// Human readable task name.
    fn name(&self) -> &'static str;

    /// Execute the task.
    fn execute(&self, context: &mut Context) -> Result<()>;
}
