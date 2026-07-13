use std::{fs, path::PathBuf};

use anyhow::{bail, Result};

use crate::{Case, Context, Experiment, Runner, Task};

/// Help text for the flags [`RunOptions::from_args`] understands, for
/// embedding in a driver's usage message.
pub const FLAGS_HELP: &str =
"  --dry-run       print the commands without executing
  --post-only     skip the solvers; run only the post-process
                  scripts against data already on disk
  --missing-only  run only cases whose output folder is
                  missing or empty (delete a case folder to
                  mark it for rerun)
  --case <name>   run only the named case; a 1-based index
                  works too (repeatable, combines with
                  --post-only)";

/// Driver-side CLI options shared by every experiment package:
/// which cases to run, whether to run solvers or only the
/// post-processing, and dry-run mode.
#[derive(Default)]
pub struct RunOptions {
    pub dry_run: bool,
    pub post_only: bool,
    pub missing_only: bool,
    /// Case selectors: exact case names or 1-based indices.
    pub cases: Vec<String>,
}

impl RunOptions {
    /// Parse driver CLI arguments (everything after the binary name).
    /// Returns the positional experiment name, if any, alongside the
    /// parsed options; `Err` carries a message describing the
    /// unknown or malformed argument.
    pub fn from_args<I>(args: I) -> std::result::Result<(Option<String>, Self), String>
    where
        I: IntoIterator<Item = String>,
    {
        let mut opts = Self::default();
        let mut name = None;

        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--dry-run" => opts.dry_run = true,
                "--post-only" => opts.post_only = true,
                "--missing-only" => opts.missing_only = true,
                "--case" => match it.next() {
                    Some(value) => opts.cases.push(value),
                    None => return Err("--case needs a value".to_string()),
                },
                _ if arg.starts_with("--") => {
                    return Err(format!("unknown option: {arg}"));
                }
                _ if name.is_none() => name = Some(arg),
                _ => return Err(format!("unexpected argument: {arg}")),
            }
        }

        Ok((name, opts))
    }

    /// Run `experiment` with these options applied. `output_directory`
    /// is the experiment's output root, holding one subfolder per case
    /// — it decides which cases already have data, and receives the
    /// run.sh/report.json bookkeeping. Post-only runs deliberately skip
    /// that bookkeeping so the originals from the real run (e.g. GPU
    /// timings) survive.
    pub fn run<E: Experiment>(&self, experiment: &E, output_directory: PathBuf) -> Result<()> {
        let all_cases = experiment.cases();

        // Resolve each --case selector: exact case name, or 1-based
        // index into the experiment's case list. A mistyped selector
        // must fail loudly, not run zero cases.
        let mut selected: Vec<String> = Vec::new();
        for want in &self.cases {
            if all_cases.iter().any(|case| &case.name == want) {
                selected.push(want.clone());
            } else if let Some(case) = want
                .parse::<usize>()
                .ok()
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| all_cases.get(i))
            {
                selected.push(case.name.clone());
            } else {
                let names: Vec<&str> = all_cases.iter().map(|c| c.name.as_str()).collect();
                bail!(
                    "no case named '{want}' in {} (cases: {})",
                    experiment.name(),
                    names.join(", ")
                );
            }
        }

        if self.missing_only {
            for case in &all_cases {
                if case_has_output(&output_directory, &case.name) {
                    println!("skipping case {} (output already exists)", case.name);
                }
            }
        }

        if self.post_only {
            for case in &all_cases {
                if !case_has_output(&output_directory, &case.name) {
                    println!("skipping case {} (no output data to post-process)", case.name);
                }
            }
        }

        let selection = Selection {
            inner: experiment,
            out: output_directory.clone(),
            only: &selected,
            missing_only: self.missing_only,
            post_only: self.post_only,
        };

        let mut ctx = Context::default();

        if self.post_only {
            let runner = Runner::new().dry_run(self.dry_run);
            runner.run(&selection, &mut ctx)
        } else {
            // A filtered run must not replace the full experiment's
            // run.sh with its partial script (report.json is safe
            // either way — the runner merges it).
            let filtered = !selected.is_empty() || self.missing_only;
            let runner = Runner::new()
                .output_directory(output_directory)
                .preserve_existing_script(filtered)
                .dry_run(self.dry_run);
            runner.run(&selection, &mut ctx)
        }
    }
}

/// A case counts as "has output" when its folder under the experiment
/// output root exists and is non-empty.
fn case_has_output(out: &PathBuf, name: &str) -> bool {
    fs::read_dir(out.join(name))
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// View of an experiment with the CLI selectors applied:
///   --case          keep only the named cases
///   --missing-only  keep only cases whose output folder is missing/empty
///   --post-only     strip the pre-process and solver tasks, keep post
struct Selection<'a, E: Experiment> {
    inner: &'a E,
    out: PathBuf,
    only: &'a [String],
    missing_only: bool,
    post_only: bool,
}

impl<E: Experiment> Experiment for Selection<'_, E> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn pre_process(&self) -> Vec<Task> {
        if self.post_only {
            Vec::new()
        } else {
            self.inner.pre_process()
        }
    }

    fn cases(&self) -> Vec<Case> {
        self.inner
            .cases()
            .into_iter()
            .filter(|case| self.only.is_empty() || self.only.contains(&case.name))
            .filter(|case| !self.missing_only || !case_has_output(&self.out, &case.name))
            // Post-only can only work on cases that have data; skip the
            // rest (e.g. a case that was intentionally never run) so the
            // remaining posts and the cross-case comparison still happen.
            .filter(|case| !self.post_only || case_has_output(&self.out, &case.name))
            .map(|mut case| {
                if self.post_only {
                    case.pre_process.clear();
                    case.run.clear();
                }
                case
            })
            .collect()
    }

    fn post_process(&self) -> Vec<Task> {
        self.inner.post_process()
    }
}
