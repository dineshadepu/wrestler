use crate::Task;

#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,

    pub pre_process: Vec<Task>,
    pub run: Vec<Task>,
    pub post_process: Vec<Task>,
}

impl Case {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pre_process: Vec::new(),
            run: Vec::new(),
            post_process: Vec::new(),
        }
    }

    pub fn pre_process(mut self, task: Task) -> Self {
        self.pre_process.push(task);
        self
    }

    pub fn run(mut self, task: Task) -> Self {
        self.run.push(task);
        self
    }

    pub fn post_process(mut self, task: Task) -> Self {
        self.post_process.push(task);
        self
    }
}
