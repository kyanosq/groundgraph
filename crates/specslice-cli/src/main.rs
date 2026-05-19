//! SpecSlice CLI entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Debug, Parser)]
#[command(
    name = "specslice",
    version,
    about = "Explicit trace-driven context layer for AI coding."
)]
struct Cli {
    /// Repository root that hosts `.specslice.yaml` and `.specslice/`.
    #[arg(long, global = true, default_value = ".")]
    repo_root: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialise a SpecSlice workspace: create `.specslice.yaml` and `.specslice/graph.db`.
    Init,
    /// Index docs and code into the graph store.
    Index(IndexArgs),
    /// Resolve a requirement into docs, implementation and tests.
    Slice(SliceArgs),
    /// Export the current graph store to a portable bundle.
    Export(ExportArgs),
}

#[derive(Debug, clap::Args)]
struct SliceArgs {
    /// The requirement ID (e.g. `REQ-WATERMARK-001`).
    requirement: String,
    /// Output a machine-readable JSON document instead of human text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct IndexArgs {
    /// Only index documentation (Markdown). Useful before any code adapter is ready.
    #[arg(long)]
    docs_only: bool,
}

#[derive(Debug, clap::Args)]
struct ExportArgs {
    /// Output format. MVP-0 only supports `jsonl`.
    #[arg(long, value_enum, default_value_t = ExportFormatArg::Jsonl)]
    format: ExportFormatArg,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ExportFormatArg {
    Jsonl,
}

impl From<ExportFormatArg> for specslice_engine::ExportFormat {
    fn from(value: ExportFormatArg) -> Self {
        match value {
            ExportFormatArg::Jsonl => specslice_engine::ExportFormat::Jsonl,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("specslice: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => commands::init::run(&cli.repo_root),
        Commands::Index(args) => commands::index::run(&cli.repo_root, args.docs_only),
        Commands::Slice(args) => commands::slice::run(&cli.repo_root, &args.requirement, args.json),
        Commands::Export(args) => commands::export::run(&cli.repo_root, args.format.into()),
    }
}
