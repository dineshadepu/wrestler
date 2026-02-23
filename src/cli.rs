use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wrestler")]
#[command(about = "HPC experiment runner", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run a problem on a target
    Run {
        problem: String,

        #[arg(long)]
        target: String,

        #[arg(long)]
        dry_run: bool,
    },

    /// View logs
    Logs {
        problem: String,

        #[arg(long)]
        target: String,

        #[arg(long)]
        run: Option<String>,

        #[arg(long)]
        phase: Option<String>,
    },
}
