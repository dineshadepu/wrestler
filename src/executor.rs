use anyhow::{anyhow, Result};
use std::fs;
use std::process::Command;

use crate::config::Target;
use crate::planner::{ConcretePhase, RunPlan};

fn ensure_directory(path: &str, target: &Target) -> Result<()> {
    if let Some(ssh) = &target.ssh {
        Command::new("ssh")
            .arg(ssh)
            .arg(format!("mkdir -p {}", path))
            .status()?;
    } else {
        std::fs::create_dir_all(path)?;
    }

    Ok(())
}

pub fn execute_run(
    problem_name: &str,
    target_name: &str,
    plan: &RunPlan,
    target: &Target,
    dry_run: bool,
) -> Result<()> {
    println!("\n====================================================");
    println!("[{} | {} | {}]", problem_name, target_name, plan.name);
    println!("====================================================");

    if dry_run {
        print_plan(problem_name, target_name, plan);
        return Ok(());
    }

    create_directories(plan, target)?;

    if let Some(build) = &plan.build {
        execute_phase("build", build, &plan.run_root, target)?;
    }

    execute_phase("run", &plan.run, &plan.run_root, target)?;

    if let Some(analyze) = &plan.analyze {
        execute_phase("analyze", analyze, &plan.run_root, target)?;
    }

    Ok(())
}

fn print_plan(problem: &str, target: &str, plan: &RunPlan) {
    println!("(dry-run)");

    if let Some(build) = &plan.build {
        print_phase(problem, target, &plan.name, "build", build);
    }

    print_phase(problem, target, &plan.name, "run", &plan.run);

    if let Some(analyze) = &plan.analyze {
        print_phase(problem, target, &plan.name, "analyze", analyze);
    }
}

fn print_phase(problem: &str, target: &str, run: &str, phase_name: &str, phase: &ConcretePhase) {
    println!("\n[{} | {} | {} | {}]", problem, target, run, phase_name);

    let shell = build_shell_string(&phase.program, &phase.args, &phase.cwd);
    println!("> {}", shell);
}

fn create_directories(plan: &RunPlan, target: &Target) -> Result<()> {
    let logs_dir = format!("{}/logs", plan.run_root);
    let run_dir = format!("{}/run", plan.run_root);
    let analysis_dir = format!("{}/analysis", plan.run_root);

    if let Some(ssh) = &target.ssh {
        let cmd = format!("mkdir -p {} {} {}", logs_dir, run_dir, analysis_dir);

        Command::new("ssh").arg(ssh).arg(cmd).status()?;
    } else {
        fs::create_dir_all(&logs_dir)?;
        fs::create_dir_all(&run_dir)?;
        fs::create_dir_all(&analysis_dir)?;
    }

    Ok(())
}

fn execute_phase(
    phase_name: &str,
    phase: &ConcretePhase,
    run_root: &str,
    target: &Target,
) -> Result<()> {
    ensure_directory(&phase.cwd, target)?;

    println!("\n--- Executing phase: {} ---", phase_name);

    let shell = build_shell_string(&phase.program, &phase.args, &phase.cwd);

    println!("> {}", shell);

    let output = if let Some(ssh) = &target.ssh {
        Command::new("ssh").arg(ssh).arg(&shell).output()?
    } else {
        Command::new("sh").arg("-c").arg(&shell).output()?
    };

    let logs_dir = format!("{}/logs", run_root);
    let stdout_path = format!("{}/{}.stdout", logs_dir, phase_name);
    let stderr_path = format!("{}/{}.stderr", logs_dir, phase_name);

    if let Some(ssh) = &target.ssh {
        // Write logs remotely
        write_remote_log(ssh, &stdout_path, &output.stdout)?;
        write_remote_log(ssh, &stderr_path, &output.stderr)?;
    } else {
        fs::write(&stdout_path, &output.stdout)?;
        fs::write(&stderr_path, &output.stderr)?;
    }

    if !output.status.success() {
        return Err(anyhow!("Phase '{}' failed", phase_name));
    }

    Ok(())
}

fn build_shell_string(program: &str, args: &[String], cwd: &str) -> String {
    let args_joined = args.join(" ");
    format!("cd {} && {} {}", cwd, program, args_joined)
}

fn write_remote_log(ssh: &str, path: &str, content: &[u8]) -> Result<()> {
    let mut child = Command::new("ssh")
        .arg(ssh)
        .arg(format!("cat > {}", path))
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    use std::io::Write;

    if let Some(stdin) = &mut child.stdin {
        stdin.write_all(content)?;
    }

    child.wait()?;

    Ok(())
}
