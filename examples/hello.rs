use anyhow::Result;
use wrestler::{Case, Context, Experiment, Runner, Task};

struct DamBreak;

impl Experiment for DamBreak {
    fn name(&self) -> &'static str {
        "dam_break"
    }

    fn pre_process(&self) -> Vec<Task> {
        vec![
            Task::new("Create Output Directory")
                .executable("mkdir")
                .arg("-p")
                .arg("outputs/dam_break"),
        ]
    }

    fn cases(&self) -> Vec<Case> {
        let base = Case::new("dx_0.002")
            .run(
                Task::new("Run Solver")
                    .executable("echo")
                    .arg("solving with")
                    .arg("--dx=0.002"),
            )
            .post_process(
                Task::new("Post Process")
                    .executable("echo")
                    .arg("post processing dx=0.002"),
            );

        // A refined case is just a clone with different arguments.
        let mut fine = base.clone();
        fine.name = "dx_0.001".into();
        fine.run[0].args[1] = "--dx=0.001".into();
        fine.post_process[0].args[0] = "post processing dx=0.001".into();

        vec![base, fine]
    }

    fn post_process(&self) -> Vec<Task> {
        vec![
            Task::new("Compare Cases")
                .executable("echo")
                .arg("comparing all cases"),
        ]
    }
}

fn main() -> Result<()> {
    let experiment = DamBreak;

    let mut ctx = Context::default();
    let runner = Runner::new()
        .output_directory("outputs/dam_break")
        .dry_run(std::env::args().any(|arg| arg == "--dry-run"));

    runner.run(&experiment, &mut ctx)
}
