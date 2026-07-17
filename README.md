# wrestler

A small Rust library for driving reproducible computational experiments:
running an external solver binary over a set of parameter cases, post-
processing each case's output, and comparing cases against each other —
with a standard CLI (`--dry-run`, `--force`, `--post-only`, `--case`),
a reproducible `run.sh`, and a `report.json` of what ran and how long it
took.

It doesn't know anything about SPH, CFD, or any particular solver.
It knows how to run shell commands in a defined order, skip work that's
already done, and keep a record of what happened. Everything
domain-specific — which binary, which flags, which resolutions, which
plotting script — lives in the package that uses it.

## Why this exists

kanaaluValidate's validation packages (`wcsph_fluid`, `wcsph_solid_dynamics`,
`khayyer_2024_solid_dynamics`, ...) all follow the same shape: build a
solver binary, run it once per case (a resolution, a parameter sweep) into
its own output folder, run a per-case post-process script, then overlay
all cases in a cross-case comparison. Re-running the whole thing every
time you tweak a plot script is wasteful, and hand-rolled shell scripts
for this get unwieldy fast — especially once you're pulling results back
from a GPU pod and need to re-plot without the solver. wrestler is the
piece that's identical across all of those packages, factored out so each
package's `main.rs` only has to describe *what* to run, not *how*.

## Core concepts

- **`Task`** — one shell command: a name (for logs/reports), an
  executable, a working directory, and args. `Task::execute()` runs it,
  streaming stdout/stderr to the terminal while also capturing them.
  `Task::to_shell()` renders it as a snippet of a reproduction script.

- **`Case`** — a named group of tasks in three stages: `pre_process` (e.g.
  `mkdir -p` the case's output folder), `run` (the solver), and
  `post_process` (e.g. a plotting script for that case). One `Case` is
  typically one resolution or one parameter combination.

- **`Experiment`** (trait) — `name()`, `cases()` (the `Vec<Case>` above),
  plus an experiment-level `pre_process()` (runs once, before any case —
  e.g. rebuild the binary, snapshot machine specs) and `post_process()`
  (runs once, after every case — e.g. a cross-case comparison plot).
  `cases()` is the only required method.

- **`Runner`** — executes an `Experiment`: `pre_process()`, then each
  case's `pre_process` → `run` → `post_process` in order, then the
  experiment's `post_process()`. Builds a `RunReport` (durations, exit
  codes, success) as it goes, and — if given an `output_directory` —
  writes `run.sh` (a standalone bash reproduction of the whole run),
  `report.json`, and per-task stdout/stderr logs.

- **`RunOptions`** — parses the driver CLI (`--dry-run`, `--force`/`-f`,
  `--post-only`, `--case <name>`) and wraps an `Experiment` with that
  behavior (lazy skip-if-already-run, case filtering, post-only) before
  handing it to a `Runner`. This is what every package's `main.rs`
  actually uses — see below.

- **`Context`** — currently an empty placeholder passed through `Runner`,
  reserved for state that turns out to be needed across tasks later
  (repo paths, executables, etc.). Most packages never touch it.

## Quick start

The minimal shape, with no CLI handling — see `examples/hello.rs`
(`cargo run --example hello`):

```rust
use anyhow::Result;
use wrestler::{Case, Context, Experiment, Runner, Task};

struct DamBreak;

impl Experiment for DamBreak {
    fn name(&self) -> &'static str {
        "dam_break"
    }

    fn cases(&self) -> Vec<Case> {
        vec![Case::new("dx_0.002").run(
            Task::new("Run Solver")
                .executable("./solver")
                .arg("--dx=0.002"),
        )]
    }
}

fn main() -> Result<()> {
    let mut ctx = Context::default();
    let runner = Runner::new().output_directory("outputs/dam_break");
    runner.run(&DamBreak, &mut ctx)
}
```

## The real pattern: `RunOptions`

Every actual validation package skips `Runner` directly and goes through
`RunOptions`, which adds the CLI and the lazy/force/post-only behavior.
The shape (trimmed from `khayyer_2024_solid_dynamics/src/main.rs`):

```rust
use std::{env, path::PathBuf, process::exit};
use wrestler::{Case, Experiment, RunOptions, Task, FLAGS_HELP};

struct UniaxialCompression {
    root: PathBuf,
}

impl UniaxialCompression {
    fn exe(&self) -> PathBuf {
        self.root.join("build/examples/pkg_uniaxial_compression")
    }
    fn out(&self) -> PathBuf {
        self.root.join("outputs/uniaxial_compression/mac")
    }
}

impl Experiment for UniaxialCompression {
    fn name(&self) -> &'static str {
        "uniaxial_compression"
    }

    fn pre_process(&self) -> Vec<Task> {
        vec![Task::new("Rebuild binaries")
            .executable("cmake")
            .arg("--build").arg(self.root.join("build").display().to_string())
            .arg("-j").arg("8")]
    }

    // After every case has run, overlay them all in one comparison figure.
    fn post_process(&self) -> Vec<Task> {
        vec![Task::new("Cross-case comparison")
            .executable("python3")
            .arg(self.root.join("examples/post_uniaxial_compression_comparison.py").display().to_string())
            .arg("--no-show")
            .working_directory(self.out())]
    }

    fn cases(&self) -> Vec<Case> {
        ["0.0005", "0.001", "0.002"].into_iter().map(|dx| {
            let dir = self.out().join(format!("dx_{dx}"));
            Case::new(format!("dx_{dx}"))
                .pre_process(Task::new("mkdir").executable("mkdir").arg("-p").arg(dir.display().to_string()))
                .run(Task::new("Run solver").executable(self.exe()).working_directory(dir.clone())
                     .args(["--dx", dx]))
                .post_process(Task::new("Post-process").executable("python3")
                     .arg(self.root.join("examples/post_uniaxial_compression.py").display().to_string())
                     .arg("--no-show").args(["--dx", dx]).working_directory(dir))
        }).collect()
    }
}

fn main() -> anyhow::Result<()> {
    let (name, opts) = match RunOptions::from_args(env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(message) => { eprintln!("{message}\n{FLAGS_HELP}"); exit(1) }
    };
    let Some(name) = name else { eprintln!("{FLAGS_HELP}"); exit(1) };

    match name.as_str() {
        "uniaxial_compression" => {
            let e = UniaxialCompression { root: env::current_dir()? };
            opts.run(&e, e.out())
        }
        _ => { eprintln!("unknown experiment: {name}"); exit(1) }
    }
}
```

Run it with `cargo run uniaxial_compression`, `cargo run uniaxial_compression --dry-run`,
`cargo run uniaxial_compression --case dx_0.001 --force`, etc.

Across the actual packages this gets factored further: an `exe_path()`
helper for the binary name, a `machine()` helper reading `$MACHINE` so
results pulled from a GPU pod land in their own subtree
(`outputs/<experiment>/<machine>/<case>`), and a shared `make_case()`
building the pre_process/run/post_process triple from `(name, args,
post_script)` — see any `src/main.rs` under kanaaluValidate for the full
pattern.

## CLI flags (via `RunOptions`)

```
--dry-run       print the commands without executing
--force, -f     rerun cases even when their output folder already
                has files (without it, such cases are skipped —
                delete a case folder to mark it for rerun)
--post-only     skip the solvers; run only the post-process
                scripts against data already on disk
--case <name>   run only the named case; a 1-based index
                works too (repeatable, combines with --force
                and --post-only)
```

This text is available as `wrestler::FLAGS_HELP` for embedding in a
driver's own `usage()`.

## Behavior notes

- **Lazy by default.** A case whose output folder already has files is
  skipped unless `--force` is given. Delete the case's folder (or the
  whole experiment's output folder) to mark it for rerun. This makes
  `cargo run <experiment>` after adding one new case cheap: only the new
  case runs.

- **`--case` accepts a name or a 1-based index**, and can be repeated.
  Combines with `--force` (`--case dx_2mm --force` reruns just that one
  case) and `--post-only` (`--case 1 --post-only` re-plots just that one).

- **`--post-only` is best-effort.** A case with no data on disk (e.g.
  after a `clean_outputs.sh` that deleted snapshots, or one that was
  never run) is reported and skipped rather than aborting the whole
  post-process pass; the other cases and the experiment-level
  `post_process()` (cross-case comparison) still run. The overall exit
  code still reflects the failure.

- **`--post-only` never touches `run.sh`/`report.json` bookkeeping** from
  the real run, so GPU timings and the reproduction script survive a
  later "just re-plot" pass.

- **`report.json` is merged, not overwritten.** A partial run (one
  re-run case, or `--case`-filtered) replaces only the records for the
  cases/stages it actually executed; everything else in the existing
  report is kept. Each case folder also gets its own
  `<output>/<case>/report.json` — a duplicate slice of just that case's
  records — so the folder is self-contained when pulled off a remote
  machine.

- **`run.sh` is a real, standalone reproduction script** (`ROOT="$(pwd)"`
  at the top, every task as `cd ...; <command> <args>`), regenerated on
  every *full* run (no `--case` filter, `--force`). A filtered/partial
  run leaves an existing `run.sh` untouched, so it keeps describing the
  full experiment rather than being clobbered by a partial one. A first
  run into an empty output tree always writes it, since there's nothing
  to preserve yet.

- **Per-task logs.** Each task's stdout/stderr, if non-empty, is written
  to `<output_directory>/logs/<NN>_<case>_<task-name>.{stdout,stderr}.log`.

- **A non-zero exit from a task fails the run** (unless
  `continue_on_case_failure` is set, as `--post-only` does internally) —
  `Runner` stops and returns an `Err`, but whatever already ran is still
  recorded in `report.json`.

## Testing

`cargo test` covers `Task::to_shell()`'s directory-anchoring rules
(relative working directories are anchored to `$ROOT`, absolute ones used
as-is, empty ones fall back to `$ROOT` itself).
