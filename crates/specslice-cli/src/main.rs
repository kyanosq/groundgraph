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
    about = "Non-invasive context layer for AI coding."
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
    /// Report which requirements, docs and tests are affected by a git diff.
    Impact(ImpactArgs),
    /// Run consistency checks (broken links, missing linked test, orphan REQ).
    Check(CheckArgs),
    /// Produce an agent-ready context pack for a requirement.
    Context(ContextArgs),
    /// Bridge AI candidate links into the confirmed graph.
    Connect(ConnectArgs),
    /// Export the current graph store to a portable bundle.
    Export(ExportArgs),
    /// Render the graph as JSON, Mermaid, or self-contained HTML.
    Graph(GraphArgs),
    /// 审阅 AI 业务候选：list / show / review。
    Candidate(CandidateArgs),
    /// 输出业务逻辑可信度报告（confirmed / candidate / stale / missing 等）。
    Logic(LogicArgs),
}

#[derive(Debug, clap::Args)]
struct GraphArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = GraphFormatArg::Html)]
    format: GraphFormatArg,
    /// Default visible surface: overview (modules), code (modules+drilldown),
    /// business (REQ subgraph) or focus (focus + 1-hop neighbourhood).
    #[arg(long, value_enum, default_value_t = GraphViewArg::Overview)]
    view: GraphViewArg,
    /// Where to write the rendered output. Defaults to stdout for JSON and
    /// Mermaid, and to `.specslice/export/graph.html` for HTML.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Focus on a single business id (`REQ-…`), module path or full
    /// artifact id.
    #[arg(long)]
    focus: Option<String>,
    /// Hide check/risk findings from the export. Defaults to true (include).
    #[arg(long, default_value_t = true)]
    include_risks: bool,
    /// Overlay the `.specslice/candidates/business_logic.yaml` candidates
    /// (and any human-accepted candidates promoted into the confirmed
    /// graph) on top of the structural view. Defaults to true so the
    /// confirmed loop is visible by default; pass `--include-candidates=false`
    /// to hide the AI overlay.
    #[arg(long, default_value_t = true)]
    include_candidates: bool,
    /// Cap the number of nodes; emits a `graph_truncated` finding when hit.
    #[arg(long)]
    max_nodes: Option<usize>,
    /// Pretty-print the JSON output.
    #[arg(long)]
    pretty: bool,
    /// Show framework-noise edges (toString / dispose / initState / build /
    /// hashCode / …). Off by default; on means "give me the full noisy graph".
    #[arg(long, default_value_t = false)]
    include_noise: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum GraphFormatArg {
    Json,
    Html,
    Mermaid,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum GraphViewArg {
    Overview,
    Code,
    Business,
    Focus,
}

impl From<GraphFormatArg> for commands::graph::GraphFormat {
    fn from(value: GraphFormatArg) -> Self {
        match value {
            GraphFormatArg::Json => commands::graph::GraphFormat::Json,
            GraphFormatArg::Html => commands::graph::GraphFormat::Html,
            GraphFormatArg::Mermaid => commands::graph::GraphFormat::Mermaid,
        }
    }
}

impl From<GraphViewArg> for specslice_engine::graph::GraphView {
    fn from(value: GraphViewArg) -> Self {
        match value {
            GraphViewArg::Overview => specslice_engine::graph::GraphView::Overview,
            GraphViewArg::Code => specslice_engine::graph::GraphView::Code,
            GraphViewArg::Business => specslice_engine::graph::GraphView::Business,
            GraphViewArg::Focus => specslice_engine::graph::GraphView::Focus,
        }
    }
}

#[derive(Debug, clap::Args)]
struct CandidateArgs {
    #[command(subcommand)]
    sub: CandidateSub,
}

#[derive(Debug, Subcommand)]
enum CandidateSub {
    /// 列出所有待审 (或全部) 业务候选。
    List(CandidateListArgs),
    /// 查看单个候选的完整业务描述、证据、风险。
    Show(CandidateShowArgs),
    /// 写回审阅结果到 `.specslice/candidates/business_logic.yaml`。
    Review(CandidateReviewArgs),
}

#[derive(Debug, clap::Args)]
struct CandidateListArgs {
    /// 包含已接受 / 已拒绝的候选。
    #[arg(long)]
    all: bool,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct CandidateShowArgs {
    /// 候选 id。
    id: String,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct CandidateReviewArgs {
    /// 候选 id。
    id: String,
    /// 接受 (accepted)。
    #[arg(long, group = "verdict")]
    accept: bool,
    /// 拒绝 (rejected)。
    #[arg(long, group = "verdict")]
    reject: bool,
    /// 需要补充 (needs_changes)。
    #[arg(long = "needs-changes", group = "verdict")]
    needs_changes: bool,
    /// 待定 (pending)。
    #[arg(long, group = "verdict")]
    pending: bool,
    /// 审阅人 (默认读取 $USER)。
    #[arg(long)]
    reviewer: Option<String>,
    /// 审阅备注。
    #[arg(long)]
    note: Option<String>,
    /// 标记为「已回答」的 open question 文本；可重复传入多次以累积。
    #[arg(long = "answer")]
    answers: Vec<String>,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct LogicArgs {
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
    /// 仅列出存在风险 (非 confirmed_link) 的条目。
    #[arg(long)]
    only_risks: bool,
}

#[derive(Debug, clap::Args)]
struct ConnectArgs {
    #[command(subcommand)]
    sub: ConnectSub,
}

#[derive(Debug, Subcommand)]
enum ConnectSub {
    /// Emit an evidence pack (JSON) describing requirements, orphan symbols
    /// and orphan tests. Pipe this to an AI to generate candidate links.
    Propose(ConnectProposeArgs),
    /// Validate AI-generated candidates and merge accepted ones into
    /// `.specslice/links.yaml`.
    Apply(ConnectApplyArgs),
}

#[derive(Debug, clap::Args)]
struct ConnectProposeArgs {
    /// Write the evidence pack to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Pretty-print the JSON output.
    #[arg(long)]
    pretty: bool,
}

#[derive(Debug, clap::Args)]
struct ConnectApplyArgs {
    /// Path to the AI-generated candidates YAML.
    #[arg(long)]
    candidates: PathBuf,
    /// Validate only — do not modify `.specslice/links.yaml`.
    #[arg(long)]
    dry_run: bool,
    /// Emit a machine-readable JSON outcome.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct CheckArgs {
    /// Output a machine-readable JSON document.
    #[arg(long)]
    json: bool,
    /// Exit with code 1 even when only warnings are reported.
    #[arg(long)]
    fail_on_warning: bool,
}

#[derive(Debug, clap::Args)]
struct ContextArgs {
    /// The requirement ID (e.g. `REQ-WATERMARK-001`).
    requirement: String,
    /// Skip inlining doc/code/test source snippets.
    #[arg(long)]
    no_snippets: bool,
    /// Output a machine-readable JSON document.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct ImpactArgs {
    /// Base git ref to diff against (default: `origin/main`).
    #[arg(long, default_value = "origin/main")]
    base: String,
    /// Head git ref (default: `HEAD`).
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// Output a machine-readable JSON document.
    #[arg(long)]
    json: bool,
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
        Commands::Impact(args) => {
            commands::impact::run(&cli.repo_root, &args.base, &args.head, args.json)
        }
        Commands::Check(args) => {
            let exit = commands::check::run(&cli.repo_root, args.json, args.fail_on_warning)?;
            if exit != 0 {
                std::process::exit(exit);
            }
            Ok(())
        }
        Commands::Context(args) => commands::context::run(
            &cli.repo_root,
            &args.requirement,
            !args.no_snippets,
            args.json,
        ),
        Commands::Connect(args) => match args.sub {
            ConnectSub::Propose(p) => {
                commands::connect::run_propose(&cli.repo_root, p.out.as_deref(), p.pretty)
            }
            ConnectSub::Apply(a) => {
                let exit =
                    commands::connect::run_apply(&cli.repo_root, a.candidates, a.dry_run, a.json)?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                Ok(())
            }
        },
        Commands::Export(args) => commands::export::run(&cli.repo_root, args.format.into()),
        Commands::Candidate(args) => match args.sub {
            CandidateSub::List(a) => {
                let mode = if a.all {
                    commands::candidate::ListMode::All
                } else {
                    commands::candidate::ListMode::Pending
                };
                commands::candidate::run_list(&cli.repo_root, mode, a.json)
            }
            CandidateSub::Show(a) => {
                let exit = commands::candidate::run_show(&cli.repo_root, &a.id, a.json)?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                Ok(())
            }
            CandidateSub::Review(a) => {
                let status = if a.accept {
                    specslice_engine::ReviewStatus::Accepted
                } else if a.reject {
                    specslice_engine::ReviewStatus::Rejected
                } else if a.needs_changes {
                    specslice_engine::ReviewStatus::NeedsChanges
                } else if a.pending {
                    specslice_engine::ReviewStatus::Pending
                } else {
                    anyhow::bail!(
                        "必须给出 --accept / --reject / --needs-changes / --pending 之一"
                    );
                };
                let reviewer = a.reviewer.clone().or_else(|| std::env::var("USER").ok());
                let exit = commands::candidate::run_review(
                    &cli.repo_root,
                    &a.id,
                    commands::candidate::ReviewArgs {
                        status,
                        reviewer: reviewer.as_deref(),
                        note: a.note.as_deref(),
                        answered: a.answers.clone(),
                        json: a.json,
                    },
                )?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                Ok(())
            }
        },
        Commands::Logic(args) => {
            let exit = commands::logic::run(&cli.repo_root, args.json, args.only_risks)?;
            if exit != 0 {
                std::process::exit(exit);
            }
            Ok(())
        }
        Commands::Graph(args) => commands::graph::run(commands::graph::GraphRunArgs {
            repo_root: cli.repo_root.clone(),
            format: args.format.into(),
            view: args.view.into(),
            out: args.out,
            focus: args.focus,
            include_risks: args.include_risks,
            include_candidates: args.include_candidates,
            max_nodes: args.max_nodes,
            pretty: args.pretty,
            include_noise: args.include_noise,
        }),
    }
}
