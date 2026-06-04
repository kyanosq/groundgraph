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
    /// 业务模块证据包 — 把代码/文档/测试事实按业务模块聚合，产出供 AI
    /// 生成 `business_logic.yaml` 候选的证据 + 中文提示词（非侵入，只读图）。
    Propose(ProposeArgs),
    /// 代码图搜索 — `grep` 的代码图替代品。
    Search(SearchArgs),
    /// 死代码报告 — 标注无法从任何入口点可达的代码符号。
    /// 不会自动删除任何文件。
    #[command(name = "dead-code")]
    DeadCode(DeadCodeArgs),
    /// 相似代码候选 — 结构层指纹比对 (P18 tier 1)。
    /// 报告结构完全相同的函数 / 方法簇，不会自动合并或删除。
    Similar(SimilarArgs),
    /// 测试选择 — 给定 diff 输出应当运行的测试列表 (P19)。
    /// 不会自动执行任何测试，仅给出带原因 / 置信度的清单。
    #[command(name = "select-tests")]
    SelectTests(SelectTestsArgs),
    /// 功能区聚类 — 启发式归纳代码图里的功能簇 (P19)。
    /// 仅作为浏览入口；不是权威功能划分。
    Features(FeaturesArgs),
    /// Graph 快照比对 — 对比两份 `.specslice/graph.db` (P19)。
    /// 调用方需自己保存 base / head 的图文件（CI artefact 等）。
    #[command(name = "graph-diff")]
    GraphDiff(GraphDiffArgs),
    /// AI 澄清问题包 — 列出代码图里需要人 / Agent 确认的事实 (P19)。
    Questions(QuestionsArgs),
}

#[derive(Debug, clap::Args)]
struct GraphDiffArgs {
    /// 基准图数据库路径。
    #[arg(long, value_name = "PATH")]
    base_db: std::path::PathBuf,
    /// 目标图数据库路径。
    #[arg(long, value_name = "PATH")]
    head_db: std::path::PathBuf,
    /// 可选：基准仓库根路径。与 `--head-root` 同时提供时，graph-diff
    /// 会额外读取两边的 `.specslice/candidates/business_logic.yaml`
    /// 并报告业务候选 added / removed / 状态变更。
    #[arg(long = "base-root", value_name = "PATH")]
    base_repo_root: Option<std::path::PathBuf>,
    /// 可选：目标仓库根路径。见 `--base-root`。
    #[arg(long = "head-root", value_name = "PATH")]
    head_repo_root: Option<std::path::PathBuf>,
    /// 输出格式：`text`、`json`。
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    format: String,
}

#[derive(Debug, clap::Args)]
struct QuestionsArgs {
    /// 每个类别最多输出的问题数（默认 20）。
    #[arg(long, value_name = "N", default_value_t = 20)]
    max_per_category: usize,
    /// 输出格式：`text`、`json`。
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    format: String,
}

#[derive(Debug, clap::Args)]
struct FeaturesArgs {
    /// 输出的最大簇数（默认 20）。
    #[arg(long, value_name = "N", default_value_t = 20)]
    max_clusters: usize,
    /// 标签传播的最大 BFS 深度（默认 3）。
    #[arg(long, value_name = "N", default_value_t = 3)]
    max_depth: usize,
    /// 簇内最小节点数，低于此值不报告（默认 3）。
    #[arg(long, value_name = "N", default_value_t = 3)]
    min_cluster_size: usize,
    /// 输出格式：`text`、`json`。
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    format: String,
}

#[derive(Debug, clap::Args)]
struct SelectTestsArgs {
    /// 基准分支 / commit (默认 main)。
    #[arg(long, default_value = "main")]
    base: String,
    /// 目标分支 / commit (默认 HEAD)。
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// 让算法沿反向 `Calls` / `References` 边再走几步，
    /// 把间接依赖的测试也纳入候选。默认关闭，因为
    /// 信号完整度依赖代码图本身的质量。
    #[arg(long)]
    include_deps: bool,
    /// `--include-deps` 模式下反向 BFS 的最大深度（默认 2）。
    #[arg(long, value_name = "N", default_value_t = 2)]
    max_depth: usize,
    /// 输出格式：`text`、`json`。
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    format: String,
}

#[derive(Debug, clap::Args)]
struct SimilarArgs {
    /// 仅返回包含此 symbol id 的相似簇。
    #[arg(long, value_name = "SYMBOL_ID")]
    node: Option<String>,
    /// 函数体最少 normalized token 数，低于此值忽略（默认 12）。
    #[arg(long, value_name = "N", default_value_t = specslice_engine::similarity::DEFAULT_MIN_TOKENS)]
    min_tokens: usize,
    /// 单个簇内最少成员数（默认 2，任意重复都报告）。
    #[arg(long, value_name = "N", default_value_t = 2)]
    min_cluster_size: usize,
    /// 检测层级：`exact` 仅结构指纹、`near` 仅 SimHash 近似、`all` 两者（默认）。
    #[arg(long, value_name = "MODE", default_value = "all")]
    mode: String,
    /// 近似簇的最低 SimHash 相似度（仅在 `near` / `all` 模式生效，默认 0.85）。
    #[arg(long, value_name = "FLOAT", default_value_t = specslice_engine::similarity::DEFAULT_MIN_SIMILARITY)]
    min_score: f32,
    /// SimHash shingle 宽度（默认 5）。
    #[arg(long, value_name = "K", default_value_t = specslice_engine::similarity::DEFAULT_SHINGLE_K)]
    shingle_k: usize,
    /// 进入 O(N²) 近似比对的最大符号数；超过该值跳过 near tier 并在 stats 标记。默认 20000。
    #[arg(long, value_name = "N", default_value_t = specslice_engine::similarity::DEFAULT_MAX_PAIRWISE_SYMBOLS)]
    max_pairwise: usize,
    /// 输出格式：`text`、`json`。
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    format: String,
}

#[derive(Debug, clap::Args)]
struct DeadCodeArgs {
    /// 最低置信度过滤：`high` / `medium` / `low`。默认 `medium`。
    #[arg(long, value_enum, default_value_t = DeadCodeConfidenceArg::Medium)]
    min_confidence: DeadCodeConfidenceArg,
    /// 同时分析孤儿测试节点（默认不报告测试本身，只把测试作为入口点）。
    #[arg(long)]
    include_tests: bool,
    /// 输出 JSON（默认为人类可读的中文文本）。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum DeadCodeConfidenceArg {
    High,
    Medium,
    Low,
}

impl DeadCodeConfidenceArg {
    fn into_engine(self) -> specslice_engine::dead_code::DeadCodeConfidence {
        use specslice_engine::dead_code::DeadCodeConfidence;
        match self {
            DeadCodeConfidenceArg::High => DeadCodeConfidence::High,
            DeadCodeConfidenceArg::Medium => DeadCodeConfidence::Medium,
            DeadCodeConfidenceArg::Low => DeadCodeConfidence::Low,
        }
    }
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
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
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
    /// 输出 JSON。等价于 `--format json`。
    #[arg(long)]
    json: bool,
    /// P14 — 输出格式。`mermaid` 会渲染“业务描述 → evidence files/
    /// symbols/tests”的局部 flowchart，状态会映射为已接受 (Confirmed)
    /// 或候选 (Candidate) 形状。
    #[arg(long, value_enum, default_value_t = CandidateShowFormatArg::Text)]
    format: CandidateShowFormatArg,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CandidateShowFormatArg {
    Text,
    Json,
    Mermaid,
}

impl CandidateShowFormatArg {
    fn into_command_format(self) -> commands::candidate::CandidateShowFormat {
        match self {
            CandidateShowFormatArg::Text => commands::candidate::CandidateShowFormat::Text,
            CandidateShowFormatArg::Json => commands::candidate::CandidateShowFormat::Json,
            CandidateShowFormatArg::Mermaid => commands::candidate::CandidateShowFormat::Mermaid,
        }
    }
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
struct ProposeArgs {
    /// 输出格式：`json`（默认，证据包，喂给 AI）/ `markdown`（可读业务文档草稿
    /// + 内嵌提示词）/ `text`（人类速览）。
    #[arg(long, value_enum, default_value_t = ProposeFormatArg::Json)]
    format: ProposeFormatArg,
    /// 写入文件而非 stdout（如 `.specslice/export/business-pack.md`）。
    #[arg(long)]
    out: Option<PathBuf>,
    /// `--format json` 时美化输出。
    #[arg(long)]
    pretty: bool,
    /// 最多报告多少个业务模块（按信号分降序，默认 40）。
    #[arg(long, value_name = "N", default_value_t = 40)]
    max_modules: usize,
    /// 每个模块最多列出多少入口符号（默认 8）。
    #[arg(long, value_name = "N", default_value_t = 8)]
    max_entry_points: usize,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ProposeFormatArg {
    Json,
    Markdown,
    Text,
}

impl ProposeFormatArg {
    fn into_command_format(self) -> commands::propose::ProposeFormat {
        match self {
            ProposeFormatArg::Json => commands::propose::ProposeFormat::Json,
            ProposeFormatArg::Markdown => commands::propose::ProposeFormat::Markdown,
            ProposeFormatArg::Text => commands::propose::ProposeFormat::Text,
        }
    }
}

#[derive(Debug, clap::Args)]
struct SearchArgs {
    /// 自由文本查询（关键词）。与 `--code` / `--file` 互斥。
    query: Option<String>,
    /// 代码片段输入：从中确定性地提取 identifier / 字符串 / 路径段
    /// 作为关键词（CLI 不做 AI 扩词）。
    #[arg(long)]
    code: Option<String>,
    /// 精确位置入口：文件路径（配合 `--line`）。
    #[arg(long)]
    file: Option<String>,
    /// 精确位置入口：行号。
    #[arg(long)]
    line: Option<u32>,
    /// 子图扩展跳数（默认 1）。0 表示只返回命中节点。
    #[arg(long, default_value_t = 1)]
    depth: usize,
    /// 直接命中数上限（不影响 1-hop 邻居数量）。
    #[arg(long, default_value_t = 25)]
    limit: usize,
    /// 按节点种类过滤命中。可重复或逗号分隔：
    /// `--kind method,class,test`。同时接受 `dart_method` / `method`
    /// 等别名。
    #[arg(long, value_delimiter = ',')]
    kind: Vec<String>,
    /// 输出格式：`text`（默认）/ `json` / `html` / `mermaid`。
    /// - `html` 渲染搜索驱动的单文件阅读器；
    /// - `mermaid` 输出 `flowchart LR` 子图，适合 PR / 设计文档。
    #[arg(long, value_enum, default_value_t = SearchFormatArg::Text)]
    format: SearchFormatArg,
    /// 兼容别名 — 等价于 `--format json`。
    #[arg(long)]
    json: bool,
    /// `--format html` 的默认写入路径为
    /// `<repo_root>/.specslice/export/search-<slug>.html`；当
    /// `--format mermaid` 时直接写入指定路径（默认打印到 stdout）。
    #[arg(long)]
    output: Option<PathBuf>,
    /// 保留 framework 噪声 calls（toString / build / dispose / …）。
    /// 默认过滤。
    #[arg(long)]
    include_noise: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum SearchFormatArg {
    Text,
    Json,
    Html,
    /// P14 — emit a Mermaid `flowchart LR` of the search subgraph,
    /// suitable for embedding in PR descriptions or design docs.
    Mermaid,
}

impl SearchFormatArg {
    fn into_command_format(self) -> commands::search::SearchFormat {
        match self {
            SearchFormatArg::Text => commands::search::SearchFormat::Text,
            SearchFormatArg::Json => commands::search::SearchFormat::Json,
            SearchFormatArg::Html => commands::search::SearchFormat::Html,
            SearchFormatArg::Mermaid => commands::search::SearchFormat::Mermaid,
        }
    }
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
    /// Diff `--base` against the current working tree instead of a committed
    /// head, so `impact` can run on uncommitted changes without a throwaway
    /// commit. `--head` is ignored when set. Tracked (staged/unstaged) edits
    /// are included; brand-new untracked files are not (git diff semantics).
    #[arg(long)]
    worktree: bool,
    /// Output a machine-readable JSON document. Equivalent to
    /// `--format json` and kept for back-compat.
    #[arg(long)]
    json: bool,
    /// 输出格式：`text`（默认）/ `json` / `mermaid`。`mermaid`
    /// 渲染 "changed files → impacted business → suggested tests" 的
    /// 局部 `flowchart LR`，边来自 `ImpactReport.impact_edges`
    /// （真实图边），不是合成近似关系。
    #[arg(long, value_enum, default_value_t = ImpactFormatArg::Text)]
    format: ImpactFormatArg,
    /// `--format mermaid` 的输出文件路径；省略时打印到 stdout。
    #[arg(long)]
    output: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ImpactFormatArg {
    Text,
    Json,
    Mermaid,
}

impl ImpactFormatArg {
    fn into_command_format(self) -> commands::impact::ImpactFormat {
        match self {
            ImpactFormatArg::Text => commands::impact::ImpactFormat::Text,
            ImpactFormatArg::Json => commands::impact::ImpactFormat::Json,
            ImpactFormatArg::Mermaid => commands::impact::ImpactFormat::Mermaid,
        }
    }
}

#[derive(Debug, clap::Args)]
struct SliceArgs {
    /// The requirement ID (e.g. `REQ-WATERMARK-001`).
    requirement: String,
    /// Output a machine-readable JSON document instead of human text.
    #[arg(long)]
    json: bool,
    /// P14 — how many hops to follow `Calls` / `References` from each
    /// declared implementation symbol. `0` recovers the manifest-only
    /// slice.
    #[arg(long, default_value_t = 1)]
    call_depth: usize,
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
    reset_sigpipe();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("specslice: {err:#}");
            ExitCode::from(1)
        }
    }
}

/// Restore the default `SIGPIPE` disposition so piping output into `head`,
/// `less`, etc. terminates the process quietly instead of panicking on the
/// next broken-pipe `stdout` write. Rust installs `SIG_IGN` for `SIGPIPE` at
/// startup, which turns a closed reader into an `ErrorKind::BrokenPipe` panic
/// deep inside `println!` (observed as a confusing exit code 101). A search /
/// report tool that advertises itself as a `grep` replacement must compose
/// cleanly in shell pipelines, so we opt back into the conventional behaviour.
/// The `sigpipe` crate encapsulates the platform `unsafe`, keeping this crate
/// within the workspace `unsafe_code = "forbid"` policy; it is a no-op on
/// non-Unix targets.
fn reset_sigpipe() {
    sigpipe::reset();
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => commands::init::run(&cli.repo_root),
        Commands::Index(args) => commands::index::run(&cli.repo_root, args.docs_only),
        Commands::Slice(args) => commands::slice::run(
            &cli.repo_root,
            &args.requirement,
            args.json,
            args.call_depth,
        ),
        Commands::Impact(args) => {
            let format = if args.json {
                commands::impact::ImpactFormat::Json
            } else {
                args.format.into_command_format()
            };
            // Empty head ref tells the engine to diff `--base` against the
            // working tree (uncommitted changes) rather than a committed range.
            let head = if args.worktree {
                ""
            } else {
                args.head.as_str()
            };
            commands::impact::run(&cli.repo_root, &args.base, head, format, args.output)
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
                let format = if a.json {
                    commands::candidate::CandidateShowFormat::Json
                } else {
                    a.format.into_command_format()
                };
                let exit = commands::candidate::run_show(&cli.repo_root, &a.id, format)?;
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
        Commands::Propose(args) => commands::propose::run(commands::propose::ProposeRunArgs {
            repo_root: cli.repo_root.clone(),
            format: args.format.into_command_format(),
            out: args.out,
            pretty: args.pretty,
            max_modules: args.max_modules,
            max_entry_points: args.max_entry_points,
        }),
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
        Commands::Search(args) => {
            // `--json` legacy flag wins when explicit; otherwise honour `--format`.
            let format = if args.json {
                commands::search::SearchFormat::Json
            } else {
                args.format.into_command_format()
            };
            commands::search::run(commands::search::SearchRunArgs {
                repo_root: cli.repo_root.clone(),
                query: args.query,
                code: args.code,
                file: args.file,
                line: args.line,
                depth: args.depth,
                limit: args.limit,
                kinds: args.kind,
                format,
                output: args.output,
                include_noise: args.include_noise,
            })
        }
        Commands::DeadCode(args) => {
            commands::dead_code::run(commands::dead_code::DeadCodeRunArgs {
                repo_root: cli.repo_root.clone(),
                min_confidence: args.min_confidence.into_engine(),
                include_tests: args.include_tests,
                json: args.json,
            })
        }
        Commands::Similar(args) => commands::similar::run(commands::similar::SimilarRunArgs {
            repo_root: cli.repo_root.clone(),
            focus_symbol_id: args.node,
            min_tokens: args.min_tokens,
            min_cluster_size: args.min_cluster_size,
            mode: args.mode,
            min_similarity: args.min_score,
            shingle_k: args.shingle_k,
            max_pairwise: args.max_pairwise,
            format: args.format,
        }),
        Commands::SelectTests(args) => {
            commands::select_tests::run(commands::select_tests::SelectTestsRunArgs {
                repo_root: cli.repo_root.clone(),
                base_ref: args.base,
                head_ref: args.head,
                include_dependent: args.include_deps,
                max_propagation_depth: args.max_depth,
                format: args.format,
            })
        }
        Commands::Features(args) => commands::features::run(commands::features::FeaturesRunArgs {
            repo_root: cli.repo_root.clone(),
            max_clusters: args.max_clusters,
            max_propagation_depth: args.max_depth,
            min_cluster_size: args.min_cluster_size,
            format: args.format,
        }),
        Commands::GraphDiff(args) => {
            commands::graph_diff::run(commands::graph_diff::GraphDiffRunArgs {
                base_db: args.base_db,
                head_db: args.head_db,
                base_repo_root: args.base_repo_root,
                head_repo_root: args.head_repo_root,
                format: args.format,
            })
        }
        Commands::Questions(args) => {
            commands::questions::run(commands::questions::QuestionsRunArgs {
                repo_root: cli.repo_root.clone(),
                max_per_category: args.max_per_category,
                format: args.format,
            })
        }
    }
}
