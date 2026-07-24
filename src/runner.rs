use anyhow::{bail, Result};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{Context, Experiment, RunReport, Task, TaskRecord};

#[derive(Default)]
pub struct Runner {
    output_directory: Option<PathBuf>,
    dry_run: bool,
    preserve_script: bool,
    continue_on_case_failure: bool,
    machine: String,
}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Directory where `run.sh`, `report.json` and per-task logs are
    /// written. Without it, `run()` executes but persists nothing.
    pub fn output_directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.output_directory = Some(dir.into());
        self
    }

    /// When enabled, `run()` prints every command in execution order
    /// instead of executing anything. `run.sh` is still written if an
    /// output directory is set; no report or logs are produced.
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// When enabled, an existing `run.sh` is left untouched — a
    /// filtered run's partial script would otherwise replace the full
    /// experiment's reproduction script.
    pub fn preserve_existing_script(mut self, preserve: bool) -> Self {
        self.preserve_script = preserve;
        self
    }

    /// When enabled, a failing case doesn't abort the experiment: the
    /// failure is reported, the remaining cases and the experiment's
    /// post-process still run, and `run()` returns an error at the end.
    /// Used by best-effort modes like post-only, where one case with
    /// unusable data shouldn't block the others.
    pub fn continue_on_case_failure(mut self, enable: bool) -> Self {
        self.continue_on_case_failure = enable;
        self
    }

    /// Machine label recorded in `report.json` (see [`RunReport::machine`]),
    /// e.g. "mac" or "runpod_a100". Purely informational — has no effect
    /// on where anything is written; the caller decides that via
    /// `output_directory`. Left empty (the default) if never set.
    pub fn machine(mut self, machine: impl Into<String>) -> Self {
        self.machine = machine.into();
        self
    }

    pub fn write_script<E: Experiment>(
        &self,
        experiment: &E,
        filename: impl AsRef<Path>,
    ) -> Result<()> {
        let mut file = File::create(filename)?;
        self.render_script(experiment, &mut file)
    }

    fn render_script<E: Experiment>(&self, experiment: &E, file: &mut impl Write) -> Result<()> {
        writeln!(file, "#!/bin/bash")?;
        writeln!(file)?;
        writeln!(file, "# Experiment: {}", experiment.name())?;
        writeln!(file)?;
        writeln!(file, "set -e")?;
        writeln!(file)?;
        writeln!(file, "ROOT=\"$(pwd)\"")?;
        writeln!(file)?;

        for task in experiment.pre_process() {
            self.write_task(file, &task)?;
        }

        for case in experiment.cases() {
            writeln!(file)?;
            writeln!(file, "#####################################")?;
            writeln!(file, "# Case: {}", case.name)?;
            writeln!(file, "#####################################")?;
            writeln!(file)?;

            self.write_tasks(file, &case.pre_process)?;
            self.write_tasks(file, &case.run)?;
            self.write_tasks(file, &case.post_process)?;
        }

        for task in experiment.post_process() {
            self.write_task(file, &task)?;
        }

        Ok(())
    }

    fn write_tasks(&self, file: &mut impl Write, tasks: &[Task]) -> Result<()> {
        for task in tasks {
            self.write_task(file, task)?;
        }

        Ok(())
    }

    fn write_task(&self, file: &mut impl Write, task: &Task) -> Result<()> {
        writeln!(file, "# {}", task.name)?;
        writeln!(file, "{}", task.to_shell())?;

        Ok(())
    }

    pub fn run<E: Experiment>(&self, experiment: &E, _ctx: &mut Context) -> Result<()> {
        println!("Experiment: {}", experiment.name());

        if let Some(dir) = &self.output_directory {
            fs::create_dir_all(dir)?;
            let script = dir.join("run.sh");
            if !(self.preserve_script && script.exists()) {
                self.write_script(experiment, script)?;
            }
        }

        if self.dry_run {
            println!("Dry run: commands that would execute, in order.");
            println!();
            return self.render_script(experiment, &mut std::io::stdout());
        }

        let start = Instant::now();
        let mut report = RunReport::new(experiment.name());
        report.machine = self.machine.clone();

        let outcome = self.run_stages(experiment, &mut report);

        report.total_duration_seconds = start.elapsed().as_secs_f64();
        report.success = outcome.is_ok();

        if let Some(dir) = &self.output_directory {
            let path = dir.join("report.json");
            let report = Self::merge_report(&path, report);
            fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!();
            println!("Report: {}", path.display());
        }

        outcome
    }

    /// Merge this run's report into an existing `report.json`, so a
    /// partial run (e.g. one re-run case) does not wipe the records of
    /// cases it did not touch: records belonging to the cases (and
    /// experiment-level stages) executed now replace their old
    /// counterparts; everything else is kept.
    fn merge_report(path: &Path, new: RunReport) -> RunReport {
        let old = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<RunReport>(&text).ok())
            .filter(|old| old.experiment == new.experiment);

        let Some(old) = old else { return new };

        let executed: HashSet<Option<String>> =
            new.tasks.iter().map(|t| t.case.clone()).collect();

        let mut tasks: Vec<TaskRecord> = old
            .tasks
            .into_iter()
            .filter(|t| !executed.contains(&t.case))
            .collect();
        tasks.extend(new.tasks);

        RunReport {
            experiment: new.experiment,
            // Both reports live under the same output_directory, so they
            // share a machine by construction; the new run's value wins.
            machine: new.machine,
            success: new.success && tasks.iter().all(|t| t.success),
            // A merged report spans several invocations, so the sum of
            // its task durations is the only meaningful total.
            total_duration_seconds: tasks.iter().map(|t| t.duration_seconds).sum(),
            tasks,
        }
    }

    fn run_stages<E: Experiment>(&self, experiment: &E, report: &mut RunReport) -> Result<()> {
        self.execute(&experiment.pre_process(), None, "pre_process", report)?;

        let mut failed_cases: Vec<String> = Vec::new();

        for case in experiment.cases() {
            println!();
            println!("=================================");
            println!("Case: {}", case.name);
            println!("=================================");

            let first = report.tasks.len();
            let outcome = self.run_case(&case, report);

            // Written even when the case failed, so the record of what
            // happened travels with the case folder.
            let saved = self.save_case_report(experiment.name(), &case.name, &report.tasks[first..]);
            let scripted = self.save_case_script(&case);

            if let Err(error) = outcome {
                if !self.continue_on_case_failure {
                    return Err(error);
                }
                println!("case {} failed ({error:#}); continuing", case.name);
                failed_cases.push(case.name.clone());
            }
            saved?;
            scripted?;
        }

        self.execute(&experiment.post_process(), None, "post_process", report)?;

        if !failed_cases.is_empty() {
            bail!("case(s) failed: {}", failed_cases.join(", "));
        }

        Ok(())
    }

    fn run_case(&self, case: &crate::Case, report: &mut RunReport) -> Result<()> {
        self.execute(&case.pre_process, Some(&case.name), "pre_process", report)?;
        self.execute(&case.run, Some(&case.name), "run", report)?;
        self.execute(&case.post_process, Some(&case.name), "post_process", report)
    }

    /// Duplicate the case's slice of the report into its own output
    /// folder (`<output_directory>/<case>/report.json`), so a case
    /// folder is self-contained: its timing record travels with it and
    /// is replaced exactly when the case is re-run.
    fn save_case_report(
        &self,
        experiment: &str,
        case: &str,
        records: &[TaskRecord],
    ) -> Result<()> {
        let Some(dir) = &self.output_directory else {
            return Ok(());
        };

        let case_dir = dir.join(case);
        if records.is_empty() || !case_dir.is_dir() {
            return Ok(());
        }

        let report = RunReport {
            experiment: experiment.to_string(),
            machine: self.machine.clone(),
            success: records.iter().all(|t| t.success),
            total_duration_seconds: records.iter().map(|t| t.duration_seconds).sum(),
            tasks: records.to_vec(),
        };

        fs::write(
            case_dir.join("report.json"),
            serde_json::to_string_pretty(&report)?,
        )?;

        Ok(())
    }

    /// Duplicate this case's own commands into
    /// `<output_directory>/<case>/run.sh`, so a case folder is
    /// self-contained: pulling just that folder (e.g. from a GPU machine)
    /// tells you exactly what was run to produce it, without needing the
    /// full experiment's `run.sh`. Mirrors `save_case_report` below —
    /// written after the case runs (so its own `pre_process` has already
    /// created the case folder) and replaced exactly when the case is
    /// re-run.
    fn save_case_script(&self, case: &crate::Case) -> Result<()> {
        let Some(dir) = &self.output_directory else {
            return Ok(());
        };

        let case_dir = dir.join(&case.name);
        if !case_dir.is_dir() {
            return Ok(());
        }

        let mut file = File::create(case_dir.join("run.sh"))?;
        self.render_case_script(case, &mut file)
    }

    fn render_case_script(&self, case: &crate::Case, file: &mut impl Write) -> Result<()> {
        writeln!(file, "#!/bin/bash")?;
        writeln!(file)?;
        writeln!(file, "# Case: {}", case.name)?;
        writeln!(file)?;
        writeln!(file, "set -e")?;
        writeln!(file)?;
        writeln!(file, "ROOT=\"$(pwd)\"")?;
        writeln!(file)?;

        self.write_tasks(file, &case.pre_process)?;
        self.write_tasks(file, &case.run)?;
        self.write_tasks(file, &case.post_process)?;

        Ok(())
    }

    fn execute(
        &self,
        tasks: &[Task],
        case: Option<&str>,
        stage: &str,
        report: &mut RunReport,
    ) -> Result<()> {
        for task in tasks {
            let result = task.execute()?;

            self.save_logs(report.tasks.len(), case, task, &result)?;

            report.tasks.push(TaskRecord {
                case: case.map(String::from),
                stage: stage.to_string(),
                task: task.name.clone(),
                duration_seconds: result.duration.as_secs_f64(),
                exit_code: result.status.code(),
                success: result.success(),
            });

            if !result.success() {
                bail!("Task '{}' failed with {}.", task.name, result.status);
            }

            println!(
                "✓ Completed in {:.3} seconds",
                result.duration.as_secs_f64()
            );
        }

        Ok(())
    }

    fn save_logs(
        &self,
        index: usize,
        case: Option<&str>,
        task: &Task,
        result: &crate::TaskResult,
    ) -> Result<()> {
        let Some(dir) = &self.output_directory else {
            return Ok(());
        };

        let logs = dir.join("logs");
        fs::create_dir_all(&logs)?;

        let slug: String = case
            .map(|c| format!("{c}_"))
            .unwrap_or_default()
            .chars()
            .chain(task.name.chars())
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
            .collect();

        for (kind, content) in [("stdout", &result.stdout), ("stderr", &result.stderr)] {
            if !content.is_empty() {
                fs::write(logs.join(format!("{index:02}_{slug}.{kind}.log")), content)?;
            }
        }

        Ok(())
    }
}
