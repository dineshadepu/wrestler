//! Running an experiment on another machine over ssh.
//!
//! wrestler does not install or build anything remotely. The remote
//! machine is set up by hand once (cargo, the solver, Python for the
//! post-process scripts, ...); after that, wrestler only does four things:
//!
//! 1. **push**   — `rsync` the driver directory (the one `cargo run` is
//!    invoked from) to the host, minus `target/`, `outputs/`, ... and
//!    whatever `[local] exclude` in `wrestler.toml` adds;
//! 2. **launch** — start the same `cargo run <experiment> <flags>` there,
//!    detached (tmux, or `setsid nohup` when tmux is missing), so it
//!    survives the ssh connection closing;
//! 3. **status** — ask whether it is still running and show its log tail;
//! 4. **pull**   — `rsync` that machine's output folder back.
//!
//! All remote bookkeeping lives in `<remote path>/.wrestler/`:
//! `<exp>.run.sh` (what was launched), `<exp>.log` (its output),
//! `<exp>.pid` (written when it starts) and `<exp>.exit` (written only
//! when it ends — its absence is what "still running" means).

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// Name of the per-driver config file, looked up in the directory
/// `cargo run` is invoked from.
pub const CONFIG_FILE: &str = "wrestler.toml";

/// Never pushed, whatever the config says: build products, results and
/// wrestler's own bookkeeping belong to each machine separately.
const BUILTIN_EXCLUDES: &[&str] = &["/target/", "/outputs/", "/.wrestler/", "/.git/"];

/// Printed when `wrestler.toml` is missing or has no entry for a host.
const CONFIG_EXAMPLE: &str = r#"[local]
machine = "mac"                 # this machine's label (overridden by $MACHINE)
exclude = ["build/", "*.vtk"]   # not pushed (target/, outputs/, .git/ never are)
include = ["data/mesh.stl"]     # pushed even if an exclude matches

[pull]
path    = "outputs/{experiment}/{machine}"   # what --pull brings back
exclude = ["raw/"]                           # left behind unless --full

[hosts.gpu1]
ssh     = "gpu1"                              # anything `ssh` accepts; defaults to the key
path    = "~/work/kanaaluValidate/wcsph_fluid"
machine = "gpu1"                              # defaults to the key
command = "cargo run --release"               # defaults to "cargo run"
setup   = ["source ~/venv/bin/activate"]      # shell lines run before it"#;

/// What to do on the remote host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAction {
    /// `--remote <host>`: push, then launch detached.
    Launch,
    /// `--pull <host>` (`--full` also brings the pull excludes, e.g. raw/).
    Pull { full: bool },
    /// `--status <host>`.
    Status,
    /// `--stop <host>`.
    Stop,
}

/// A remote action and the `[hosts.<name>]` entry it targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRequest {
    pub action: RemoteAction,
    pub host: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub local: LocalConfig,
    #[serde(default)]
    pub pull: PullConfig,
    #[serde(default)]
    pub hosts: BTreeMap<String, HostConfig>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LocalConfig {
    /// This machine's label; see [`machine`].
    pub machine: Option<String>,
    /// rsync patterns left out of the push, on top of [`BUILTIN_EXCLUDES`].
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Paths pushed even when an exclude (built-in or not) matches them.
    #[serde(default)]
    pub include: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullConfig {
    /// Folder `--pull` copies back, relative to the driver directory on
    /// both ends. `{experiment}` and `{machine}` are substituted.
    #[serde(default = "default_pull_path")]
    pub path: String,
    /// rsync patterns a light pull leaves behind; `--full` ignores them.
    #[serde(default = "default_pull_exclude")]
    pub exclude: Vec<String>,
}

impl Default for PullConfig {
    fn default() -> Self {
        Self {
            path: default_pull_path(),
            exclude: default_pull_exclude(),
        }
    }
}

fn default_pull_path() -> String {
    "outputs/{experiment}/{machine}".to_string()
}

fn default_pull_exclude() -> Vec<String> {
    vec!["raw/".to_string()]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Destination for `ssh`/`rsync` (alias, `user@host`, ...); defaults
    /// to the table's key.
    pub ssh: Option<String>,
    /// Driver directory on the host. `~/` is the remote home.
    pub path: String,
    /// Label exported as `$MACHINE` for the remote run; defaults to the
    /// table's key.
    pub machine: Option<String>,
    /// Shell command the experiment name and flags are appended to.
    #[serde(default = "default_command")]
    pub command: String,
    /// Shell lines run (in order, in the same shell) before `command`.
    #[serde(default)]
    pub setup: Vec<String>,
}

fn default_command() -> String {
    "cargo run".to_string()
}

impl Config {
    /// Read `wrestler.toml` from `dir`. A missing file is an empty config.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(CONFIG_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }
}

/// This machine's label: `$MACHINE` if set, else `[local] machine` from
/// `./wrestler.toml`, else `"local"`.
///
/// A remote run gets `MACHINE=<[hosts.x] machine>` exported, so a driver
/// that builds its output folder from this label writes into its own
/// `outputs/<experiment>/<machine>/` on every machine — which is what
/// makes `--pull` safe to merge into the local tree.
pub fn machine() -> String {
    if let Ok(label) = std::env::var("MACHINE")
        && !label.is_empty()
    {
        return label;
    }
    std::env::current_dir()
        .ok()
        .and_then(|dir| Config::load(&dir).ok())
        .and_then(|config| config.local.machine)
        .unwrap_or_else(|| "local".to_string())
}

/// A `[hosts.*]` entry with its defaults filled in.
struct Host {
    name: String,
    ssh: String,
    path: String,
    machine: String,
    command: String,
    setup: Vec<String>,
}

impl Host {
    fn resolve(config: &Config, name: &str) -> Result<Self> {
        let Some(entry) = config.hosts.get(name) else {
            let known: Vec<&str> = config.hosts.keys().map(String::as_str).collect();
            bail!(
                "no [hosts.{name}] in {CONFIG_FILE} (known: {}). Example:\n\n{CONFIG_EXAMPLE}",
                if known.is_empty() { "none".to_string() } else { known.join(", ") }
            );
        };
        Ok(Self {
            name: name.to_string(),
            ssh: entry.ssh.clone().unwrap_or_else(|| name.to_string()),
            path: entry.path.clone(),
            machine: entry.machine.clone().unwrap_or_else(|| name.to_string()),
            command: entry.command.clone(),
            setup: entry.setup.clone(),
        })
    }
}

/// What the status probe found on the host.
#[derive(Debug, Default, PartialEq)]
struct Probe {
    /// "yes", "created" or "missing".
    dir: String,
    /// "running", "finished", "died" or "never".
    state: String,
    /// Contents of `<exp>.exit` when finished: an exit code or "stopped".
    exit: String,
    /// The first word of `command` (usually cargo) is on PATH.
    tool: bool,
    tmux: bool,
    /// The pull folder exists.
    outputs: bool,
    log_tail: String,
}

impl Probe {
    fn parse(text: &str) -> Self {
        let mut probe = Self::default();
        let mut lines = text.lines();
        for line in lines.by_ref() {
            if line == "--- log ---" {
                break;
            }
            match line.split_once('=') {
                Some(("dir", v)) => probe.dir = v.to_string(),
                Some(("state", v)) => probe.state = v.to_string(),
                Some(("exit", v)) => probe.exit = v.to_string(),
                Some(("tool", v)) => probe.tool = v == "yes",
                Some(("tmux", v)) => probe.tmux = v == "yes",
                Some(("outputs", v)) => probe.outputs = v == "yes",
                _ => {}
            }
        }
        probe.log_tail = lines.collect::<Vec<_>>().join("\n");
        probe
    }

    /// One-line human summary of `state`/`exit`.
    fn describe(&self) -> String {
        match (self.state.as_str(), self.exit.as_str()) {
            ("running", _) => "running".to_string(),
            ("finished", "0") => "finished OK".to_string(),
            ("finished", "stopped") => "stopped (--stop)".to_string(),
            ("finished", code) => format!("FAILED (exit {code})"),
            ("died", _) => "died (no exit code: killed, or the machine rebooted)".to_string(),
            _ => "never launched".to_string(),
        }
    }
}

/// Everything needed to talk to one host about one experiment.
struct Remote<'a> {
    host: Host,
    config: &'a Config,
    experiment: &'a str,
    dry_run: bool,
}

/// Carry out `request` for `experiment`. `forwarded` is the argument list
/// the remote run gets appended to its `command` (experiment name first).
pub(crate) fn execute(
    request: &RemoteRequest,
    experiment: &str,
    forwarded: &[String],
    dry_run: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    if !cwd.join(CONFIG_FILE).is_file() {
        bail!(
            "remote runs need a {CONFIG_FILE} in {} (the directory `cargo run` is \
             invoked from). Example:\n\n{CONFIG_EXAMPLE}",
            cwd.display()
        );
    }
    let config = Config::load(&cwd)?;
    let remote = Remote {
        host: Host::resolve(&config, &request.host)?,
        config: &config,
        experiment,
        dry_run,
    };

    match request.action {
        RemoteAction::Launch => remote.launch(forwarded),
        RemoteAction::Pull { full } => remote.pull(full),
        RemoteAction::Status => remote.status(),
        RemoteAction::Stop => remote.stop(),
    }
}

impl Remote<'_> {
    /// `.wrestler/<experiment>`, the prefix of every bookkeeping file.
    fn state_prefix(&self) -> String {
        format!(".wrestler/{}", self.experiment)
    }

    /// tmux rejects `.` and `:` in session names.
    fn session(&self) -> String {
        let name: String = self
            .experiment
            .chars()
            .map(|c| if c == '.' || c == ':' { '_' } else { c })
            .collect();
        format!("wr-{name}")
    }

    fn pull_path(&self) -> String {
        self.config
            .pull
            .path
            .replace("{experiment}", self.experiment)
            .replace("{machine}", &self.host.machine)
            .trim_end_matches('/')
            .to_string()
    }

    fn launch(&self, forwarded: &[String]) -> Result<()> {
        println!(
            "Remote: {} on {} ({}:{})",
            self.experiment, self.host.name, self.host.ssh, self.host.path
        );

        if !self.dry_run {
            let probe = self.probe(true)?;
            if probe.state == "running" {
                bail!(
                    "{} is already running on {} — launching again would sync new code \
                     under it and race it for the same outputs. Check it with --status {}, \
                     or end it with --stop {} first.",
                    self.experiment, self.host.name, self.host.name, self.host.name
                );
            }
            if !probe.tool {
                bail!(
                    "`{}` not found on {} (after sourcing ~/.cargo/env and `setup`). \
                     wrestler doesn't install anything: set the machine up once by hand, \
                     or fix `command`/`setup` in [hosts.{}].",
                    first_word(&self.host.command),
                    self.host.name,
                    self.host.name
                );
            }
            if !probe.tmux {
                println!("note: tmux not found on {}; falling back to setsid nohup", self.host.name);
            }
        }

        let mut push = vec!["-az".to_string()];
        push.extend(push_filters(&self.config.local));
        push.push("./".to_string());
        push.push(format!("{}:{}/", self.host.ssh, rsync_path(&self.host.path)));
        self.rsync("Push", &push)?;

        let output = self.ssh("Launch", &self.launch_script(forwarded), true)?;
        if self.dry_run {
            return Ok(());
        }

        let launcher = field(&output, "launched").unwrap_or("?");
        let pid = field(&output, "pid").unwrap_or("?");
        println!("Launched on {} via {launcher} (pid {pid}).", self.host.name);
        println!("The run no longer needs this connection. Next:");
        self.print_hints(launcher == "tmux");
        Ok(())
    }

    fn pull(&self, full: bool) -> Result<()> {
        let path = self.pull_path();
        println!(
            "Pull: {}:{}/{path}/ -> {path}/ ({})",
            self.host.ssh,
            self.host.path,
            if full { "full" } else { "light" }
        );

        if !self.dry_run {
            let probe = self.probe(false)?;
            if probe.dir == "missing" {
                bail!("{} does not exist on {}", self.host.path, self.host.name);
            }
            if !probe.outputs {
                bail!(
                    "nothing to pull: {path}/ does not exist on {} (run status: {})",
                    self.host.name,
                    probe.describe()
                );
            }
            if probe.state == "running" {
                println!("warning: still running on {} — these are partial results", self.host.name);
            }
            fs::create_dir_all(&path)?;
        }

        let mut args = vec!["-az".to_string()];
        if !full {
            args.extend(filter_args(&[], &self.config.pull.exclude));
        }
        args.push(format!("{}:{}/{path}/", self.host.ssh, rsync_path(&self.host.path)));
        args.push(format!("{path}/"));
        self.rsync("Pull outputs", &args)?;

        // The run's own log, so a failure can be read after the host is gone.
        let log_dir = format!(".wrestler/{}", self.host.name);
        if !self.dry_run {
            fs::create_dir_all(&log_dir)?;
        }
        let log = vec![
            "-az".to_string(),
            format!(
                "{}:{}/{}.log",
                self.host.ssh,
                rsync_path(&self.host.path),
                self.state_prefix()
            ),
            format!("{log_dir}/"),
        ];
        if let Err(error) = self.rsync("Pull log", &log) {
            println!("note: remote log not copied ({error:#})");
        }

        if !self.dry_run {
            println!("Pulled into {path}/.");
            println!(
                "Re-plot it here with: MACHINE={} cargo run {} --post-only",
                sh_quote(&self.host.machine),
                self.experiment
            );
        }
        Ok(())
    }

    fn status(&self) -> Result<()> {
        if self.dry_run {
            self.ssh("Status", &self.probe_script(false), false)?;
            return Ok(());
        }
        let probe = self.probe(false)?;
        println!("{} on {}: {}", self.experiment, self.host.name, probe.describe());
        if probe.dir == "missing" {
            println!("({} does not exist there yet)", self.host.path);
            return Ok(());
        }
        if !probe.log_tail.is_empty() {
            println!();
            println!("--- last lines of {}/{}.log ---", self.host.path, self.state_prefix());
            println!("{}", probe.log_tail);
            println!();
        }
        if probe.state == "running" {
            self.print_hints(probe.tmux);
        } else if probe.outputs {
            println!("Pull results with: cargo run {} --pull {}", self.experiment, self.host.name);
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if !self.dry_run {
            let probe = self.probe(false)?;
            if probe.state != "running" {
                println!(
                    "{} is not running on {} ({}); nothing to stop.",
                    self.experiment,
                    self.host.name,
                    probe.describe()
                );
                return Ok(());
            }
        }
        self.ssh("Stop", &self.stop_script(), true)?;
        if !self.dry_run {
            println!("Stopped {} on {}.", self.experiment, self.host.name);
        }
        Ok(())
    }

    fn print_hints(&self, tmux: bool) {
        let (exp, host) = (self.experiment, &self.host.name);
        println!("  cargo run {exp} --status {host}     # running? last lines of the log");
        println!("  cargo run {exp} --pull {host}       # copy results back (partial while running)");
        println!("  cargo run {exp} --stop {host}       # end it");
        if tmux {
            println!("  ssh -t {} tmux attach -t {}   # watch live (detach: Ctrl-b d)", self.host.ssh, self.session());
        }
    }

    /// Run the probe script and parse its answer. `create` makes the
    /// remote directory when it doesn't exist yet (for a first push).
    fn probe(&self, create: bool) -> Result<Probe> {
        Ok(Probe::parse(&self.ssh("Probe", &self.probe_script(create), false)?))
    }

    /// Shell lines that put the run's tools on PATH: rustup's env file
    /// (non-interactive ssh shells usually skip ~/.bashrc), then `setup`.
    fn environment(&self) -> String {
        let mut script = String::from("[ -f \"$HOME/.cargo/env\" ] && . \"$HOME/.cargo/env\"\n");
        for line in &self.host.setup {
            script.push_str(line);
            script.push('\n');
        }
        script
    }

    fn probe_script(&self, create: bool) -> String {
        let dir = remote_dir(&self.host.path);
        let state = self.state_prefix();
        let missing = if create {
            format!("mkdir -p {dir} && echo dir=created || {{ echo dir=missing; exit 0; }}")
        } else {
            "echo dir=missing; exit 0".to_string()
        };
        format!(
            r#"if [ -d {dir} ]; then echo dir=yes; else {missing}; fi
cd {dir}
S={state}
{{
{env}}} </dev/null >/dev/null 2>&1
command -v {tool} >/dev/null 2>&1 && echo tool=yes || echo tool=no
command -v tmux >/dev/null 2>&1 && echo tmux=yes || echo tmux=no
[ -d {pull} ] && echo outputs=yes || echo outputs=no
pid=$(cat "$S.pid" 2>/dev/null)
if [ -n "$pid" ] && [ ! -f "$S.exit" ] && ps -p "$pid" -o args= 2>/dev/null | grep -qF {script}; then
  echo state=running
elif [ -f "$S.exit" ]; then
  echo state=finished; echo "exit=$(cat "$S.exit")"
elif [ -n "$pid" ]; then
  echo state=died
else
  echo state=never
fi
if [ -f "$S.log" ]; then echo "--- log ---"; tail -n 20 "$S.log"; fi
true
"#,
            env = self.environment(),
            tool = sh_quote(first_word(&self.host.command)),
            pull = sh_quote(&self.pull_path()),
            script = sh_quote(&format!("{}.run.sh", self.experiment)),
        )
    }

    /// The script that runs detached on the host: records its pid, runs
    /// the experiment with its output teed to the log, then writes the
    /// exit code. A missing exit file therefore means "still running",
    /// or — once the pid is gone — "died".
    fn run_script(&self, forwarded: &[String]) -> String {
        let args: Vec<String> = forwarded.iter().map(|arg| sh_quote(arg)).collect();
        format!(
            r#"#!/bin/bash
# Written by `wrestler --remote {host}`: {exp} on {host}, detached.
cd {dir} || exit 1
S={state}
echo $$ > "$S.pid"
(
set -e
echo "wrestler: {exp} started $(date) on $(hostname)"
export MACHINE={machine}
{env}{command} {args}
) 2>&1 | tee "$S.log"
status=${{PIPESTATUS[0]}}
echo "wrestler: {exp} finished $(date) with exit code $status" >> "$S.log"
echo "$status" > "$S.exit"
"#,
            host = self.host.name,
            exp = self.experiment,
            dir = remote_dir(&self.host.path),
            state = self.state_prefix(),
            machine = sh_quote(&self.host.machine),
            env = self.environment(),
            command = self.host.command,
            args = args.join(" "),
        )
    }

    fn launch_script(&self, forwarded: &[String]) -> String {
        format!(
            r#"set -e
cd {dir}
mkdir -p .wrestler
S={state}
rm -f "$S.exit" "$S.pid"
cat > "$S.run.sh" <<'__WRESTLER_RUN_SH__'
{run}__WRESTLER_RUN_SH__
if command -v tmux >/dev/null 2>&1; then
  tmux new-session -d -s {session} -c "$PWD" "bash $(printf %q "$PWD/$S.run.sh")" </dev/null
  echo launched=tmux
elif command -v setsid >/dev/null 2>&1; then
  setsid nohup bash "$PWD/$S.run.sh" >/dev/null 2>&1 </dev/null &
  echo launched=nohup
else
  nohup bash "$PWD/$S.run.sh" >/dev/null 2>&1 </dev/null &
  echo launched=nohup
fi
for _ in $(seq 50); do [ -f "$S.pid" ] && break; sleep 0.1; done
[ -f "$S.pid" ] && echo "pid=$(cat "$S.pid")"
true
"#,
            dir = remote_dir(&self.host.path),
            state = self.state_prefix(),
            run = self.run_script(forwarded),
            session = sh_quote(&self.session()),
        )
    }

    fn stop_script(&self) -> String {
        format!(
            r#"cd {dir} || exit 0
S={state}
pid=$(cat "$S.pid" 2>/dev/null)
tmux kill-session -t {session} 2>/dev/null || true
if [ -n "$pid" ]; then kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true; fi
[ -f "$S.exit" ] || echo stopped > "$S.exit"
echo "wrestler: stopped $(date) by --stop" >> "$S.log"
"#,
            dir = remote_dir(&self.host.path),
            state = self.state_prefix(),
            session = sh_quote(&self.session()),
        )
    }

    /// Feed `script` to `bash -s` on the host and return its stdout.
    /// `stream` passes its stderr straight to the terminal instead of
    /// holding it for the error message. In dry-run mode the
    /// script is printed instead and an empty string returned.
    fn ssh(&self, label: &str, script: &str, stream: bool) -> Result<String> {
        if self.dry_run {
            println!("# {label}: ssh -T {} 'bash -s' <<'EOF'", self.host.ssh);
            print!("{script}");
            println!("EOF");
            println!();
            return Ok(String::new());
        }

        let mut child = Command::new("ssh")
            .args(["-T", &self.host.ssh, "bash -s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if stream { Stdio::inherit() } else { Stdio::piped() })
            .spawn()
            .context("failed to start `ssh` — is OpenSSH installed?")?;
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(script.as_bytes())?;
        let output = child.wait_with_output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // 255 is ssh's own failure (unreachable, auth); anything else
            // came from the script.
            if output.status.code() == Some(255) {
                bail!(
                    "{label}: could not reach {} over ssh — check `ssh {}` works on \
                     its own (keys, ~/.ssh/config).\n{}",
                    self.host.name,
                    self.host.ssh,
                    stderr.trim()
                );
            }
            bail!("{label} failed on {} ({}).\n{}", self.host.name, output.status, stderr.trim());
        }
        Ok(stdout)
    }

    fn rsync(&self, label: &str, args: &[String]) -> Result<()> {
        if self.dry_run {
            let shown: Vec<String> = args.iter().map(|arg| sh_quote(arg)).collect();
            println!("# {label}: rsync {}", shown.join(" "));
            println!();
            return Ok(());
        }
        println!("{label}...");
        let status = Command::new("rsync")
            .args(args)
            .status()
            .context("failed to start `rsync` — install it locally (and on the host)")?;
        if !status.success() {
            bail!("{label}: rsync failed ({status})");
        }
        Ok(())
    }
}

/// rsync filter options for the push: [`LocalConfig::include`] first
/// (rsync stops at the first matching rule), then the user's excludes,
/// then the built-in ones.
fn push_filters(local: &LocalConfig) -> Vec<String> {
    let mut excludes = local.exclude.clone();
    excludes.extend(BUILTIN_EXCLUDES.iter().map(|s| s.to_string()));
    filter_args(&local.include, &excludes)
}

/// Turn plain include/exclude lists into rsync `--include`/`--exclude`
/// options that mean what they say:
///
/// - An include containing a `/` is a path from the driver directory, and
///   every parent directory of it is included too — rsync never descends
///   into an excluded directory, so without the parents an include inside
///   an excluded folder would silently do nothing.
/// - A directory pattern (`foo/`) becomes `foo/***`, i.e. the folder *and
///   its contents*. For excludes that matters once a parent was
///   re-included above: excluding only the folder itself would let all its
///   other contents through.
fn filter_args(includes: &[String], excludes: &[String]) -> Vec<String> {
    let mut rules: Vec<String> = Vec::new();
    let mut push = |rule: String| {
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    };

    for include in includes {
        let trimmed = include.trim_start_matches("./").trim_start_matches('/');
        let body = trimmed.trim_end_matches('/');
        if body.contains('/') {
            let parts: Vec<&str> = body.split('/').collect();
            for depth in 1..parts.len() {
                push(format!("--include=/{}/", parts[..depth].join("/")));
            }
        }
        let anchored = if body.contains('/') || include.starts_with('/') {
            format!("/{body}")
        } else {
            body.to_string()
        };
        if trimmed.ends_with('/') {
            push(format!("--include={anchored}/***"));
        } else {
            push(format!("--include={anchored}"));
        }
    }

    for exclude in excludes {
        match exclude.strip_suffix('/') {
            Some(dir) => push(format!("--exclude={dir}/***")),
            None => push(format!("--exclude={exclude}")),
        }
    }

    rules
}

/// Single-quote `s` for a POSIX shell.
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `path` as a shell word on the host, with a leading `~` expanding to
/// the remote home (a quoted `~` would not).
fn remote_dir(path: &str) -> String {
    if path == "~" {
        "\"$HOME\"".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sh_quote(rest.trim_end_matches('/')))
    } else {
        sh_quote(path.trim_end_matches('/'))
    }
}

/// `path` for an rsync `host:path` argument: rsync paths are relative to
/// the remote home already, so `~/` is dropped instead of relying on the
/// remote shell to expand it.
fn rsync_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if path == "~" {
        ".".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

fn first_word(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

/// Value of a `key=value` line in a script's output.
fn field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn include_inside_excluded_dir_brings_its_parents_and_excludes_siblings() {
        let rules = filter_args(&strings(&["data/meshes/a.stl"]), &strings(&["data/"]));
        assert_eq!(
            rules,
            strings(&[
                "--include=/data/",
                "--include=/data/meshes/",
                "--include=/data/meshes/a.stl",
                "--exclude=data/***",
            ])
        );
    }

    #[test]
    fn directory_include_takes_its_contents() {
        let rules = filter_args(&strings(&["configs/"]), &strings(&["*.json"]));
        assert_eq!(rules, strings(&["--include=configs/***", "--exclude=*.json"]));
    }

    #[test]
    fn plain_patterns_are_left_unanchored() {
        let rules = filter_args(&strings(&["keep.vtk"]), &strings(&["*.vtk", "__pycache__/"]));
        assert_eq!(
            rules,
            strings(&["--include=keep.vtk", "--exclude=*.vtk", "--exclude=__pycache__/***"])
        );
    }

    #[test]
    fn builtin_excludes_come_after_user_rules_so_includes_can_override_them() {
        let local = LocalConfig {
            machine: None,
            exclude: strings(&["build/"]),
            include: strings(&["outputs/reference/"]),
        };
        let rules = push_filters(&local);
        assert_eq!(
            rules,
            strings(&[
                "--include=/outputs/",
                "--include=/outputs/reference/***",
                "--exclude=build/***",
                "--exclude=/target/***",
                "--exclude=/outputs/***",
                "--exclude=/.wrestler/***",
                "--exclude=/.git/***",
            ])
        );
    }

    #[test]
    fn home_paths_expand_on_the_remote_side() {
        assert_eq!(remote_dir("~/work/my dir"), "\"$HOME\"/'work/my dir'");
        assert_eq!(remote_dir("/scratch/run/"), "/scratch/run");
        assert_eq!(rsync_path("~/work/pkg/"), "work/pkg");
        assert_eq!(rsync_path("~"), ".");
    }

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(sh_quote("--kn"), "--kn");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn probe_output_is_parsed() {
        let probe = Probe::parse(
            "dir=yes\ntool=yes\ntmux=no\noutputs=yes\nstate=finished\nexit=3\n--- log ---\nline a\nx=1",
        );
        assert_eq!(probe.state, "finished");
        assert_eq!(probe.describe(), "FAILED (exit 3)");
        assert!(probe.tool && !probe.tmux && probe.outputs);
        assert_eq!(probe.log_tail, "line a\nx=1");
    }

    #[test]
    fn config_defaults_fill_in() {
        let config: Config = toml::from_str("[hosts.gpu1]\npath = \"~/pkg\"\n").unwrap();
        let host = Host::resolve(&config, "gpu1").unwrap();
        assert_eq!(host.ssh, "gpu1");
        assert_eq!(host.machine, "gpu1");
        assert_eq!(host.command, "cargo run");
        assert_eq!(config.pull.path, "outputs/{experiment}/{machine}");
        assert_eq!(config.pull.exclude, strings(&["raw/"]));
        assert!(Host::resolve(&config, "gpu2").is_err());
    }
}
