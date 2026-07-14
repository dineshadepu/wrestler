use std::{fs, path::PathBuf};

use anyhow::{bail, Result};

use crate::{Case, Context, Experiment, Runner, Task};

/// Help text for the flags [`RunOptions::from_args`] understands, for
/// embedding in a driver's usage message.
pub const FLAGS_HELP: &str =
"  --dry-run       print the commands without executing
  --force, -f     rerun cases even when their output folder already
                  has files (without it, such cases are skipped —
                  delete a case folder to mark it for rerun)
  --post-only     skip the solvers; run only the post-process
                  scripts against data already on disk
  --case <name>   run only the named case; a 1-based index
                  works too (repeatable, combines with --force
                  and --post-only)";

/// Driver-side CLI options shared by every experiment package:
/// which cases to run, whether to run solvers or only the
/// post-processing, and dry-run mode.
///
/// Solver runs are lazy by default: a case whose output folder
/// already has files is skipped unless `force` is set.
#[derive(Default)]
pub struct RunOptions {
    pub dry_run: bool,
    pub post_only: bool,
    pub force: bool,
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
                "--force" | "-f" => opts.force = true,
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

        // Solver runs are lazy: without --force, a case whose output
        // folder already has files is left alone.
        if !self.force && !self.post_only {
            for case in &all_cases {
                if case_has_output(&output_directory, &case.name) {
                    println!(
                        "skipping case {} (output exists; --force to rerun)",
                        case.name
                    );
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
            force: self.force,
            post_only: self.post_only,
        };

        let mut ctx = Context::default();

        if self.post_only {
            // Best effort: one case whose data turned out unusable
            // (e.g. snapshots deleted by clean_outputs.sh) must not
            // block the other cases or the cross-case comparison.
            let runner = Runner::new()
                .continue_on_case_failure(true)
                .dry_run(self.dry_run);
            runner.run(&selection, &mut ctx)
        } else {
            // Only a forced, unfiltered run is the "full experiment";
            // anything else must not replace the full run.sh with its
            // partial script (report.json is safe either way — the
            // runner merges it). A first run into an empty output tree
            // still writes run.sh, since there is nothing to preserve.
            let full_rerun = self.force && selected.is_empty();
            let runner = Runner::new()
                .output_directory(output_directory)
                .preserve_existing_script(!full_rerun)
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
///   --case       keep only the named cases
///   --force      also run cases whose output folder already has files
///                (without it, solver runs are lazy and skip them)
///   --post-only  strip the pre-process and solver tasks, keep post
struct Selection<'a, E: Experiment> {
    inner: &'a E,
    out: PathBuf,
    only: &'a [String],
    force: bool,
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
            // Lazy by default: a case with existing output only reruns
            // under --force (post-only instead *requires* existing
            // output; its filter is below).
            .filter(|case| {
                self.force || self.post_only || !case_has_output(&self.out, &case.name)
            })
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
