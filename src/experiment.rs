use crate::{Case, Task};

pub trait Experiment {
    fn name(&self) -> &'static str;

    fn pre_process(&self) -> Vec<Task> {
        Vec::new()
    }

    fn cases(&self) -> Vec<Case>;

    fn post_process(&self) -> Vec<Task> {
        Vec::new()
    }
}
