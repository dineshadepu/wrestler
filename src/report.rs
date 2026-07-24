use serde::{Deserialize, Serialize};

/// One executed task, as recorded in `report.json`.
#[derive(Serialize, Deserialize, Clone)]
pub struct TaskRecord {
    /// Case the task belongs to; `None` for experiment-level stages.
    pub case: Option<String>,
    pub stage: String,
    pub task: String,
    pub duration_seconds: f64,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Aggregated result of one experiment run.
#[derive(Serialize, Deserialize)]
pub struct RunReport {
    pub experiment: String,
    /// Machine label the run executed under, e.g. "mac" or "runpod_a100"
    /// (see [`crate::Runner::machine`]). Empty when the caller never set
    /// one. `#[serde(default)]` so older report.json files without this
    /// field still deserialize.
    #[serde(default)]
    pub machine: String,
    pub success: bool,
    pub total_duration_seconds: f64,
    pub tasks: Vec<TaskRecord>,
}

impl RunReport {
    pub fn new(experiment: impl Into<String>) -> Self {
        Self {
            experiment: experiment.into(),
            machine: String::new(),
            success: false,
            total_duration_seconds: 0.0,
            tasks: Vec::new(),
        }
    }
}
