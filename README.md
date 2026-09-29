# wrestler

A small Rust library for driving reproducible computational experiments:
running an external solver binary over a set of parameter cases, post-
processing each case's output, and comparing cases against each other —
with a standard CLI (`--list-cases`, `--dry-run`, `--force`, `--post-only`,
`--case`),
a reproducible `run.sh`, and a `report.json` of what ran and how long it
took. The same command can be sent to another machine over ssh
(`--remote gpu1`), and its results pulled back (`--pull gpu1`) — see
[Running on another machine over ssh](#running-on-another-machine-over-ssh).

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

- **`RunOptions`** — parses the driver CLI (`--list-cases`, `--dry-run`,
  `--force`/`-f`, `--post-only`, `--case <name>`) and wraps an `Experiment` with that
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
helper for the binary name, an output folder built from `opts.machine`
(filled from `$MACHINE` / `wrestler.toml`, see below) so results pulled
from a GPU pod land in their own subtree
(`outputs/<experiment>/<machine>/<case>`), and a shared `make_case()`
building the pre_process/run/post_process triple from `(name, args,
post_script)` — see any `src/main.rs` under kanaaluValidate for the full
pattern.

## CLI flags (via `RunOptions`)

```
--list-cases    print the experiment's cases and exit: the 1-based
                index --case accepts, the name, and which already
                have output. A query — never rebuilds, never runs.
                The summary underneath is written against the flags
                actually passed, so `--list-cases --force` reports
                what --force would do rather than advising it.
                Prefer it over --dry-run for "what cases are there?":
                dry-run applies the same laziness a real run would,
                so it hides cases that already have output unless
                --force is also given
--dry-run       print the commands without executing
--force, -f     rerun cases even when their output folder already
                has files (without it, such cases are skipped —
                delete a case folder to mark it for rerun)
--post-only     skip the solvers; run only the post-process
                scripts against data already on disk
--case <name>   run only the named case; a 1-based index
                works too (repeatable, combines with --force
                and --post-only)
-- <args>...    (or: the first flag not listed above) everything
                from here on is passed straight through to the
                solver binary, verbatim — only applied when exactly
                one case ends up running (use --case to narrow a
                multi-case experiment down to one); e.g.
                `cargo run <experiment> --case 1 --out-every 5 --kn 1e5`

Remote (hosts and sync rules come from ./wrestler.toml):
--remote <host> push this directory to <host> and start the run
                there, detached; --force/--post-only/--case and
                solver args are forwarded, --dry-run prints the
                rsync/ssh commands instead
--status <host> is it still running? exit code and log tail
--pull <host>   copy <host>'s output folder back (light: skips the
                [pull] excludes, e.g. raw/); add --full for all
--stop <host>   end a run started with --remote
```

This text is available as `wrestler::FLAGS_HELP` for embedding in a
driver's own `usage()`.

### Solver passthrough args

`RunOptions::from_args` only owns the fixed flag set above. It parses
left to right, and the moment it hits a `--flag` it doesn't recognize
(after the experiment name), it stops interpreting anything and takes
that token plus everything after it, verbatim, as `extra_args` —
without trying to figure out which of them are flags versus values. A
literal `--` does the same thing explicitly, which is the escape hatch
for a passthrough token that would otherwise collide with a wrestler
flag name (e.g. the solver has its own unrelated `--force`):

```
cargo run stack_of_cylinders --force --out-every 5 --kn 1e5
#                            ^^^^^^^ wrestler's own flag
#                                    ^^^^^^^^^^^^^^^^^^^^^^^ extra_args, forwarded to the solver
cargo run stack_of_cylinders -- --force 5   # `--force` here goes to the solver, not wrestler
```

`extra_args` is only ever appended to a case's `run` task(s) — and only
when the case list has resolved down to **exactly one** entry after
`--case` filtering and the lazy/post-only rules have run. This is a
correctness guard, not a limitation to work around: appending the same
override to every case of a multi-case sweep (e.g. an angle sweep)
would silently apply one case's intended value to all the others. If
more than one case would run, the args are dropped with a printed
warning instead of being applied to any of them — narrow the run to one
case first with `--case <name-or-index>`.

The experiment name must still come before the first passthrough
token — `cargo run --out-every 5 stack_of_cylinders` is a hard parse
error, since wrestler would have no case to attach `--out-every 5` to
yet.

## Running on another machine over ssh

Solver runs are often too long or too big for the laptop you drive them
from. wrestler can replay the exact command you would have typed locally
on another machine, detached, and bring the results back later:

```
cargo run uniaxial_compression --remote gpu1 --case 2 --force   # push + start, returns in seconds
cargo run uniaxial_compression --status gpu1                    # running? log tail
cargo run uniaxial_compression --pull gpu1                      # copy results back
MACHINE=gpu1 cargo run uniaxial_compression --post-only         # re-plot the pulled gpu1 results locally
```

### What wrestler does — and doesn't

wrestler **does not install, build or configure anything** on the remote
machine. You set it up once by hand — Rust/cargo, the solver built at
the same relative location as on your laptop, Python and whatever the
post-process scripts import — and check that a plain
`cargo run <experiment>` works there. After that, wrestler only:

1. **pushes** the driver directory (the one you run `cargo run` in) with
   `rsync`, minus `target/`, `outputs/`, `.wrestler/`, `.git/` and your
   own excludes;
2. **launches** `cd <path> && MACHINE=<machine> <command> <experiment>
   <flags>` there inside **tmux** (or `setsid nohup` when tmux isn't
   installed), then disconnects — the run belongs to the server, not to
   your ssh session, so closing the laptop doesn't touch it;
3. answers **`--status`** from the pid/exit files it keeps on the host;
4. **pulls** that machine's output folder back with `rsync`.

That split is deliberate: syncing a directory and starting a command are
stable; automating everyone's toolchain setup is where tools like this
break.

The lifecycle of one run:

```
local                                        host
─────                                        ────
cargo run exp --remote gpu1
  ├─ ssh: already running? cargo on PATH? ──▶ (refuses to double-launch)
  ├─ rsync push ────────────────────────────▶ <path>/ updated
  ├─ ssh: start detached ───────────────────▶ tmux: cargo run exp …   ─┐
  └─ returns                                   log → .wrestler/exp.log │ hours
                                                                       │
cargo run exp --status gpu1 ──── ssh ───────▶ running / finished / died│
                                                                       ▼
                                              .wrestler/exp.exit written
cargo run exp --pull gpu1 ──── rsync ◀─────── outputs/exp/gpu1/
```

### `wrestler.toml`

Lives next to the driver's `Cargo.toml` (the directory `cargo run` is
invoked from). Every key is optional except each host's `path`:

```toml
[local]
machine = "mac"                 # this machine's label (overridden by $MACHINE)
exclude = ["build/", "*.vtk"]   # not pushed (target/, outputs/, .wrestler/, .git/ never are)
include = ["data/mesh.stl"]     # pushed even if an exclude matches it

[pull]
path    = "outputs/{experiment}/{machine}"   # what --pull brings back (this is the default)
exclude = ["raw/"]                           # a light pull leaves these behind (default ["raw/"])

[hosts.gpu1]
ssh     = "gpu1"                              # anything `ssh` accepts; defaults to the key
path    = "~/work/kanaaluValidate/wcsph_fluid"
machine = "gpu1"                              # exported as $MACHINE there; defaults to the key
command = "cargo run --release"               # defaults to "cargo run"
setup   = ["module load cuda", "source ~/venv/bin/activate"]   # shell lines run first
```

- **`[local] include`/`exclude`** use rsync patterns. A pattern ending in
  `/` means a directory *and everything in it*. An include with a `/` in
  it is a path from the driver directory and wins over any exclude —
  wrestler also includes its parent directories for you, which rsync
  otherwise requires (it never descends into an excluded folder). So
  `exclude = ["data/"]` + `include = ["data/meshes/a.stl"]` pushes just
  that one file out of `data/`. Anything the solver needs that is *not*
  pushed (its build, large inputs) must already be in place on the host.
- **The push never deletes** anything on the host, and never touches the
  host's `target/` or `outputs/`: each machine keeps its own build and
  its own results.
- **`setup`** lines run in the same shell, before `command`, after
  `~/.cargo/env` is sourced (non-interactive ssh shells usually skip
  `~/.bashrc`, which is where rustup normally puts cargo on PATH).
- Use an alias from `~/.ssh/config` for `ssh`, with key-based login, so
  that `ssh gpu1` works without a prompt — wrestler makes several short
  ssh connections per command.

### Outputs from more than one machine

The same experiment run locally and on `gpu1` must never write into the
same folder. The convention that guarantees it is a machine level in the
output path, which is what `RunOptions::machine` is for:

```rust
let (name, opts) = RunOptions::from_args(env::args().skip(1)).map_err(anyhow::Error::msg)?;
let out = root.join("outputs").join(exp_name).join(&opts.machine);
opts.run(&experiment, out)
```

`opts.machine` is `$MACHINE` if set, else `[local] machine`, else
`"local"` — and a remote run gets `MACHINE=<[hosts.x] machine>` exported,
so on every machine the driver writes into its own subtree, and
`[pull] path` (by default `outputs/{experiment}/{machine}`) copies back
exactly that subtree:

```
outputs/<experiment>/
├── mac/                  ← local runs
│   ├── dx_0.001/
│   │   ├── raw/          ← solver snapshots (big; left on the host by a light pull)
│   │   ├── processed/    ← small CSV/NPZ the per-case post-process writes
│   │   ├── figures/
│   │   └── run.sh, report.json
│   └── comparison/       ← cross-case figures for this machine
├── gpu1/                 ← pulled from gpu1, same shape
└── cross_machine/        ← optional, deliberate mac-vs-gpu1 overlays
```

The split between `raw/` and `processed/` is a convention for your
post-process scripts, not something wrestler enforces — but it is what
makes a **light pull** useful: if per-case scripts write the small data
their plots need into `processed/`, and cross-case comparisons read only
`processed/`, then everything except the snapshots (a few MB instead of
GBs) is enough to re-plot and re-compare with `--post-only` long after
the host is gone. `--pull <host> --full` brings `raw/` too, when you do
need the snapshots. (Name the folder whatever you like and list it in
`[pull] exclude`.)

### Re-plotting pulled results: `MACHINE=<host>`

Every local command works on **this machine's** folder only — a plain
`cargo run <exp> --post-only` on the laptop re-plots
`outputs/<exp>/mac/`, not the `gpu1/` folder you just pulled. And it
never touches the server. To point a local command at pulled results,
override the label for that one command:

| Goal | Command |
|---|---|
| re-plot local results | `cargo run <exp> --post-only` |
| re-plot pulled gpu1 results, on this machine | `MACHINE=gpu1 cargo run <exp> --post-only` |
| see which pulled gpu1 cases have output | `MACHINE=gpu1 cargo run <exp> --list-cases` |
| re-plot on gpu1 itself | `cargo run <exp> --remote gpu1 --post-only`, then `--pull gpu1` |

`MACHINE=gpu1` only renames the folder the driver reads and writes; the
post-process scripts still run here, with this machine's Python. Avoid
it for anything but `--post-only`/`--list-cases` — a local solver run
under another machine's label would mix this machine's results into that
machine's folder.

For a manuscript, copy the chosen figures out of `outputs/` into the
manuscript's own folder rather than linking into `outputs/`, which gets
re-run and cleaned. Each case folder's `run.sh` and `report.json` say how
a figure's data was produced.

### Flags

- **`--remote <host>`** — refuses if the experiment is already running
  there (a second push would change the code under the running job) or if
  `command`'s program (e.g. `cargo`) isn't found there. Otherwise pushes,
  launches and prints the follow-up commands. `--force`, `--post-only`,
  `--case` and solver passthrough args are forwarded to the remote run
  as-is, so `--remote gpu1 --case 2 --force --kn 1e5` means exactly what
  `--case 2 --force --kn 1e5` means locally. Laziness applies there too:
  relaunching after a crash or reboot only runs the cases without output.
- **`--status <host>`** — one of *running*, *finished OK*, *FAILED (exit
  N)*, *stopped*, *died* (no exit code: killed, or the host rebooted) or
  *never launched*, followed by the last 20 lines of the run's log.
- **`--pull <host>` [`--full`]** — works while the run is still going
  (with a warning: those are partial results). Also copies the run's log
  to `.wrestler/<host>/<experiment>.log` locally.
- **`--stop <host>`** — kills the tmux session / process group and marks
  the run as stopped.
- **`--dry-run`** with any of them prints the `rsync` commands and the
  scripts that would be sent over ssh, without connecting.

To watch a tmux-launched run live: `ssh -t gpu1 tmux attach -t wr-<experiment>`
(detach again with `Ctrl-b d` — the run keeps going).

### What's on the host

Everything wrestler keeps there is in `<path>/.wrestler/`:

| File | Meaning |
|---|---|
| `<exp>.run.sh` | the exact script that was launched |
| `<exp>.log` | everything the run printed, with start/finish lines |
| `<exp>.pid` | written when the run starts |
| `<exp>.exit` | written only when it ends: exit code, or `stopped` |

A missing `.exit` with a live pid is "running"; a missing `.exit` with a
dead pid is "died".

### Not covered

Job schedulers (SLURM/PBS — on a cluster whose login nodes kill long
processes, submit a job instead), splitting one experiment's cases
across several hosts, and notifications when a run finishes.

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
  to preserve yet. Each case folder also gets its own
  `<output>/<case>/run.sh` — just that case's `pre_process`/`run`/
  `post_process` commands, standalone-runnable — written after the case
  runs and replaced exactly when that case is re-run, mirroring the
  per-case `report.json` above.

- **Per-task logs.** Each task's stdout/stderr, if non-empty, is written
  to `<output_directory>/logs/<NN>_<case>_<task-name>.{stdout,stderr}.log`.

- **A non-zero exit from a task fails the run** (unless
  `continue_on_case_failure` is set, as `--post-only` does internally) —
  `Runner` stops and returns an `Err`, but whatever already ran is still
  recorded in `report.json`.

## Testing

`cargo test` covers `Task::to_shell()`'s directory-anchoring rules
(relative working directories are anchored to `$ROOT`, absolute ones used
as-is, empty ones fall back to `$ROOT` itself), and `RunOptions::from_args`'s
passthrough parsing: known flags leave `extra_args` empty, the first
unrecognized flag after the experiment name switches to passthrough,
an explicit `--` forces passthrough even for known-looking flags, and
an unrecognized flag before the experiment name is still a hard error.
For remote runs it covers the arguments forwarded to the host, which
flags each remote action accepts, the rsync filter rules generated from
`[local] include`/`exclude`, `~` handling in remote paths, and
`wrestler.toml` defaults. The ssh/rsync side itself was checked
end to end against a local stand-in for `ssh`, not in `cargo test`.
