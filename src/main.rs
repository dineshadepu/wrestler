mod cli;
mod config;
mod executor;
mod planner;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Commands};
use config::Config;
use executor::execute_run;
use planner::plan_problem;

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration
    let config = Config::load("wrestler.toml")?;

    match cli.command {
        Commands::Run {
            problem,
            target,
            dry_run,
        } => {
            let problem_cfg = config.problems.get(&problem).expect("Problem not found");

            let target_cfg = config.targets.get(&target).expect("Target not found");

            let plans = plan_problem(&problem, problem_cfg, &target, target_cfg);

            for plan in plans {
                execute_run(&problem, &target, &plan, target_cfg, dry_run)?;
            }
        }

        Commands::Logs {
            problem,
            target,
            run,
            phase,
        } => {
            let target_cfg = config.targets.get(&target).expect("Target not found");

            handle_logs(&problem, &target, run, phase, target_cfg)?;
        }
    }

    Ok(())
}

use std::fs;
use std::process::Command;

fn handle_logs(
    problem: &str,
    target: &str,
    run: Option<String>,
    phase: Option<String>,
    target_cfg: &config::Target,
) -> Result<()> {
    let base = format!(
        "{}/wrestler_outputs/problems/{}/runs/{}",
        target_cfg.root, problem, target
    );

    if run.is_none() {
        println!("Available runs:");

        if executor::should_use_ssh(target_cfg) {
            let ssh = target_cfg.ssh.as_ref().unwrap();
            Command::new("ssh")
                .arg(ssh)
                .arg(format!("ls {}", base))
                .status()?;
        } else {
            for entry in fs::read_dir(base)? {
                let entry = entry?;
                println!(" - {}", entry.file_name().to_string_lossy());
            }
        }

        return Ok(());
    }

    let run = run.unwrap();
    let phase = phase.unwrap_or("run".to_string());

    let stdout_path = format!("{}/{}/logs/{}.stdout", base, run, phase);

    println!("\nShowing log: {}\n", stdout_path);

    if executor::should_use_ssh(target_cfg) {
        let ssh = target_cfg.ssh.as_ref().unwrap();
        Command::new("ssh")
            .arg(ssh)
            .arg(format!("cat {}", stdout_path))
            .status()?;
    } else {
        let content = fs::read_to_string(stdout_path)?;
        println!("{}", content);
    }

    Ok(())
}
