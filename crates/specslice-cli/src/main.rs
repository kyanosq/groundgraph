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
    /// 业务逻辑文档 — 把**已确认**的业务候选连同代码图中解析出的证据
    /// （代码/文档/测试/信号）渲染成可读业务文档（pipeline 后半程）。
    #[command(name = "business-doc")]
    BusinessDoc(BusinessDocArgs),
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
    /// 管理面板 — 把概览 / 业务模块 / 功能簇 / 检查 / 死代码 / 待澄清 /
    /// 纯度聚合成一个自包含的离线 HTML 文件，浏览器直接打开即可。
    Dashboard(DashboardArgs),
    /// 行为事实抽取 (P24) — 从每个代码符号体里确定性地抽出分支 / 循环 /
    /// return / 比较 / 空值 / 抛出 / await 计数与「决策证据行」，并标注纯度。
    /// 重构 / 移植时用来补足「图里没有的行为」。
    Facts(FactsArgs),
    /// 节点纯度普查 (P24) — pure / impure / unknown 计数 + 副作用原因
    /// (io / async / ui / time / randomness / global_mutation)。
    Purity(PurityArgs),
    /// 常量 / 字面量目录 (P24) — 把代码体里的 int / float / string / bool /
    /// char 字面量按值聚合（出现次数降序），定位每个魔法值的所有出现点。
    /// 移植 / 重构时用来「一个不漏地」复刻关键常量。
    Constants(ConstantsArgs),
    /// 数据契约视图 (P24) — 抽取 `CREATE TABLE` 持久化 schema（位于字符串里）
    /// 与 `obj['key'] ?? default` 形式的序列化键映射。移植时用来对齐
    /// DB 结构与 JSON 线格式 / 默认值。
    Contract(ContractArgs),
    /// 移植覆盖率账本 (P24) — 按符号名对比「源」与「目标」两份 graph.db，
    /// 报告已移植 / 缺失 / 目标独有，以及按源文件的覆盖率。用于重写时
    /// 跟踪「还差哪些没移植」。
    #[command(name = "port-coverage")]
    PortCoverage(PortCoverageArgs),
    /// 路由移植覆盖率 (P26) — 按规范化路由路径对比「消费方」(客户端 graph.db)
    /// 与「服务端」(重写方 graph.db) 的 http_route，报告已服务 / 缺失 / 服务端
    /// 独有，并按服务(路径首段)给出覆盖率。回答「客户端要的接口，重写后端还差
    /// 哪些没实现」。匹配键默认取末 2 段(controller/action)以容忍网关前缀差异。
    #[command(name = "route-coverage")]
    RouteCoverage(RouteCoverageArgs),
    /// 业务图等价 (P24+) — 把「同一业务切片」在源/目标两份 graph.db 中按
    /// 路径 glob 圈定，量化对比节点数(按种类/家族)、内部边数、名称覆盖率。
    /// JSON 输出可喂给 AI 逐子图遍历、逐项审查差异，用数字证明「Go 等价替代
    /// Java」。
    #[command(name = "graph-equiv")]
    GraphEquiv(GraphEquivArgs),
    /// 索引数据库表结构 (P25) — 扫描 .sql 的 CREATE TABLE 与 Java 实体的
    /// @TableName/@Table，把「表(含列)」写入 graph.db 作为 DbTable 节点，
    /// 让数据契约成为业务图等价的证据(graph-equiv 会对比表/列)。
    #[command(name = "schema-index")]
    SchemaIndex(SchemaIndexArgs),
    /// 从事实生成测试建议 (P24) — 基于分支 / 比较 / 空值 / 抛出 / 纯度 +
    /// 常量边界，给出每个符号的确定性单测清单（分支多者优先）。不会写测试。
    #[command(name = "suggest-tests")]
    SuggestTests(SuggestTestsArgs),
    /// 一键切片导出 (P24) — 把某个特性（按路径前缀或需求 ID 选定）所需的
    /// 全部上下文（符号+行为事实、内部/外部依赖边、常量、数据契约、测试建议）
    /// 打成一个自包含 JSON 包，交给智能体直接重写，无需再读整库。
    #[command(name = "feature-pack")]
    FeaturePack(FeaturePackArgs),
    /// 命令使用统计 (P26) — 汇总 `.specslice/stats.jsonl`：每命令调用次数 /
    /// 总+平均+最大耗时 / 失败数 / 累计指标（节点/返回/覆盖）。每次任意命令
    /// 运行都会自动追加一条记录。
    Stats(StatsArgs),
    /// 接口 → 整张图 (P27) — 从一个端点/符号出发，沿调用/引用/持久化边做前向
    /// 传递闭包，输出 controller→service→impl→mapper→SQL→table 的完整下游链路，
    /// 并汇总最终触达的表。`search` 只给 1 跳、`graph --view focus` 也偏浅；
    /// `trace` 才回答「这个接口背后牵动了图里的哪些东西」。
    Trace(TraceArgs),
}

#[derive(Debug, clap::Args)]
struct TraceArgs {
    /// 端点 / 符号名（与 `search` 同款匹配），如 `selectCraftTree`。
    #[arg(value_parser = non_empty_value)]
    query: String,
    /// 最大遍历深度（跳数），默认 12。
    #[arg(long, value_name = "N", default_value_t = 12)]
    depth: usize,
    /// 闭包节点上限，超出即截断，默认 400。
    #[arg(long, value_name = "N", default_value_t = 400)]
    max_nodes: usize,
    /// 作为起点的 search 命中数上限，默认 6。
    #[arg(long, value_name = "N", default_value_t = 6)]
    seeds: usize,
    /// 保留框架噪声调用（toString/build…），默认过滤。
    #[arg(long)]
    include_noise: bool,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct StatsArgs {
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
    /// 清空统计账本（删除 `.specslice/stats.jsonl`）。
    #[arg(long)]
    reset: bool,
}

#[derive(Debug, clap::Args)]
struct FeaturePackArgs {
    /// 以文件路径前缀选定范围（如 `lib/alarm`）。
    #[arg(long, value_name = "PREFIX")]
    path: Option<String>,
    /// 以需求 ID 选定范围（复用切片找出其触及的文件）。
    #[arg(long, value_name = "REQ")]
    requirement: Option<String>,
    /// 每个符号最多保留多少行「决策证据」。
    #[arg(long, value_name = "N", default_value_t = 12)]
    max_evidence: usize,
    /// 输出人类可读摘要（默认输出 JSON）。
    #[arg(long)]
    text: bool,
}

#[derive(Debug, clap::Args)]
struct SuggestTestsArgs {
    /// 同时为类型容器生成建议。
    #[arg(long)]
    include_types: bool,
    /// 只针对纯函数（最便宜、最确定的用例）。
    #[arg(long)]
    only_pure: bool,
    /// 优先级阈值，低于此值不输出（默认 1）。
    #[arg(long, value_name = "N", default_value_t = 1)]
    min_priority: u32,
    /// 最多输出多少个符号（0 = 不限）。
    #[arg(long, value_name = "N", default_value_t = 0)]
    max: usize,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct PortCoverageArgs {
    /// 源项目的 graph.db 路径（被移植方）。
    #[arg(long, value_name = "PATH")]
    source_db: PathBuf,
    /// 目标项目的 graph.db 路径（重写方）。
    #[arg(long, value_name = "PATH")]
    target_db: PathBuf,
    /// 不匹配类型容器，只比较可调用符号。
    #[arg(long)]
    callables_only: bool,
    /// 额外列出目标独有（源中没有）的名字。
    #[arg(long)]
    include_extra: bool,
    /// 不要过滤代码生成文件（freezed/.g.dart/l10n 等，默认会过滤）。
    #[arg(long)]
    include_generated: bool,
    /// 不要过滤测试/spec 文件（默认会过滤）。
    #[arg(long)]
    include_tests: bool,
    /// 不要过滤合成/匿名名字（如 `<default>` 构造器，默认会过滤）。
    #[arg(long)]
    include_synthetic: bool,
    /// 归一化标识符匹配：去掉前导 `_` 私有前缀，使 Dart `_foo` 命中 Swift `foo`。
    #[arg(long)]
    normalize_names: bool,
    /// 大小写不敏感匹配：Java `selectCraftTree` 命中 Go `SelectCraftTree`
    /// （C#/Pascal 同理）。跨语言移植（Java→Go）必备。
    #[arg(long)]
    ignore_case: bool,
    /// 移植映射 YAML（aliases: 源名 -> 目标名），把改名移植计入覆盖。
    #[arg(long, value_name = "PATH")]
    port_map: Option<PathBuf>,
    /// 额外排除的源/目标路径 glob（可重复，如 `**/l10n/**`）。
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,
    /// 仅源侧包含范围 glob（可重复）：把覆盖率分母收敛到源工程的某个切片，
    /// 如 `**/rcmtm-cloud-craft/**` 只统计 craft 微服务的移植进度。不影响目标侧。
    #[arg(long = "source-include", value_name = "GLOB")]
    source_include: Vec<String>,
    /// 仅源侧排除范围 glob（可重复），在 --source-include 之后施加；只从源侧分母
    /// 剔除，不会隐藏目标侧的 extra 符号。
    #[arg(long = "source-exclude", value_name = "GLOB")]
    source_exclude: Vec<String>,
    /// 列表长度上限（0 = 不限）。
    #[arg(long, value_name = "N", default_value_t = 0)]
    max: usize,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct RouteCoverageArgs {
    /// 消费方(客户端)的 graph.db 路径——这些路由「必须」被服务端实现。
    #[arg(long, value_name = "PATH")]
    source_db: PathBuf,
    /// 服务端(重写方)的 graph.db 路径——实际提供的路由。
    #[arg(long, value_name = "PATH")]
    target_db: PathBuf,
    /// 匹配键取路由末 N 段(0 = 整条路径)。默认 2 = controller/action，
    /// 容忍网关前缀差异，又不至于退化成易碰撞的纯动作名。
    #[arg(long, value_name = "N", default_value_t = specslice_engine::route_coverage::DEFAULT_SUFFIX_SEGMENTS)]
    suffix_segments: usize,
    /// 额外列出服务端独有(无消费方)的路由。
    #[arg(long)]
    include_extra: bool,
    /// 两侧都排除的路由路径 glob(可重复，如 `/token/**`)。
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,
    /// 列表长度上限(0 = 不限)。
    #[arg(long, value_name = "N", default_value_t = 0)]
    max: usize,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct GraphEquivArgs {
    /// 源项目的 graph.db 路径（被移植方，如 Java）。
    #[arg(long, value_name = "PATH")]
    source_db: PathBuf,
    /// 目标项目的 graph.db 路径（重写方，如 Go）。
    #[arg(long, value_name = "PATH")]
    target_db: PathBuf,
    /// 源切片路径 glob（可重复，如 `rcmtm-cloud-craft/**`）。留空=整图。
    #[arg(long, value_name = "GLOB")]
    source_scope: Vec<String>,
    /// 目标切片路径 glob（可重复，如 `internal/craft/**`）。留空=整图。
    #[arg(long, value_name = "GLOB")]
    target_scope: Vec<String>,
    /// 只比较可调用符号，不计入类型容器。
    #[arg(long)]
    callables_only: bool,
    /// 大小写不敏感匹配：Java `selectCraftTree` 命中 Go `SelectCraftTree`。
    #[arg(long)]
    ignore_case: bool,
    /// 归一化名字：去掉非字母数字（snake_case ↔ camelCase 对齐）。
    #[arg(long)]
    normalize_names: bool,
    /// 不过滤代码生成文件（默认过滤）。
    #[arg(long)]
    include_generated: bool,
    /// 不过滤测试/spec 文件（默认过滤）。
    #[arg(long)]
    include_tests: bool,
    /// 缺失/独有名单长度上限（0 = 不限）。
    #[arg(long, value_name = "N", default_value_t = 0)]
    max: usize,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct SchemaIndexArgs {
    /// 输出 JSON 统计。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct ContractArgs {
    /// 仅输出 CREATE TABLE schema。
    #[arg(long)]
    tables_only: bool,
    /// 仅输出序列化键映射。
    #[arg(long)]
    keys_only: bool,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct ConstantsArgs {
    /// 同时分析类型容器的体（默认只分析可调用符号）。
    #[arg(long)]
    include_types: bool,
    /// 保留 0 / 1 / 空串 / bool / char 等平凡值（默认过滤）。
    #[arg(long)]
    include_trivial: bool,
    /// 仅报告出现次数 ≥ N 的值（默认 1）。
    #[arg(long, value_name = "N", default_value_t = 1)]
    min_occurrences: usize,
    /// 仅某类字面量：`int` / `float` / `str` / `bool` / `char`。
    #[arg(long, value_name = "KIND")]
    kind: Option<String>,
    /// 最多输出多少个去重值（0 = 不限）。
    #[arg(long, value_name = "N", default_value_t = 0)]
    max: usize,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct FactsArgs {
    /// 同时分析类型容器（class / struct / enum）的体，默认只分析可调用符号。
    #[arg(long)]
    include_types: bool,
    /// 只输出指定纯度的符号：`pure` / `impure` / `unknown`。
    #[arg(long, value_name = "PURITY")]
    purity: Option<String>,
    /// 最多输出多少个符号（0 = 不限）。
    #[arg(long, value_name = "N", default_value_t = 0)]
    max: usize,
    /// 每个符号最多展示多少条证据行。
    #[arg(long, value_name = "N", default_value_t = 20)]
    max_evidence: usize,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct PurityArgs {
    /// 同时分析类型容器（class / struct / enum）的体。
    #[arg(long)]
    include_types: bool,
    /// 只列出指定纯度：`pure` / `impure` / `unknown`。
    #[arg(long, value_name = "PURITY")]
    only: Option<String>,
    /// 输出 JSON。
    #[arg(long)]
    json: bool,
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
struct DashboardArgs {
    /// 输出文件路径（默认 `.specslice/export/dashboard.html`）。
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct FeaturesArgs {
    /// 输出的最大簇数。0 = 按仓库规模自适应（约每 250 个符号 1 簇，20–80）。
    #[arg(long, value_name = "N", default_value_t = 0)]
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
    /// 基准分支 / commit (默认 `origin/main`，与 `impact` 统一)。
    // Unified with `impact` so switching between the two git-diff analyses is
    // not a trap on a fresh clone (#112).
    #[arg(long, default_value = "origin/main")]
    base: String,
    /// 目标分支 / commit (默认 HEAD)。
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// 用 `--base` 与当前工作树比较，而非已提交的 head，这样
    /// `select-tests` 也能在未提交改动上运行（与 `impact --worktree`
    /// 对齐，避免在两个 git-diff 分析之间切换时踩坑）。设置后忽略 `--head`。
    #[arg(long)]
    worktree: bool,
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
    /// Self-contained WebGL force-directed "constellation" view of the full
    /// graph topology (the `webui` viewer with the data baked in).
    Web,
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
            GraphFormatArg::Web => commands::graph::GraphFormat::Web,
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
    #[arg(value_parser = non_empty_value)]
    id: String,
    /// 输出 JSON。等价于 `--format json`，与显式 `--format` 互斥。
    #[arg(long, conflicts_with = "format")]
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
    #[arg(value_parser = non_empty_value)]
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
struct BusinessDocArgs {
    /// 输出格式：`markdown`（默认，可读业务文档）/ `json`（结构化）/
    /// `text`（速览）。
    #[arg(long, value_enum, default_value = "markdown")]
    format: BusinessDocFormatArg,
    /// 写入文件而非 stdout（如 `.specslice/export/business-doc.md`）。
    #[arg(long)]
    out: Option<PathBuf>,
    /// 同时纳入尚未确认的候选（proposed / pending / needs_changes），
    /// 在文档中标注为草稿。便于审阅未完成时预览。
    #[arg(long)]
    include_proposed: bool,
    /// 同时纳入已被拒绝的候选（默认不纳入）。
    #[arg(long)]
    include_rejected: bool,
    /// `--format json` 时美化输出。
    #[arg(long)]
    pretty: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum BusinessDocFormatArg {
    Markdown,
    Json,
    Text,
}

impl BusinessDocFormatArg {
    fn into_command_format(self) -> commands::business_doc::BusinessDocFormat {
        match self {
            BusinessDocFormatArg::Markdown => commands::business_doc::BusinessDocFormat::Markdown,
            BusinessDocFormatArg::Json => commands::business_doc::BusinessDocFormat::Json,
            BusinessDocFormatArg::Text => commands::business_doc::BusinessDocFormat::Text,
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
    /// 兼容别名 — 等价于 `--format json`，与显式 `--format` 互斥。
    #[arg(long, conflicts_with = "format")]
    json: bool,
    /// `--format html` 的默认写入路径为
    /// `<repo_root>/.specslice/export/search-<slug>.html`；当
    /// `--format mermaid` 时直接写入指定路径（默认打印到 stdout）。
    /// 显式传入的路径不受 `.specslice/` 非侵入约束——这是用户主动
    /// 指定的输出位置；省略该参数时 SpecSlice 永远只写 `.specslice/`。
    // `--out` is the canonical name across all commands (#91); `--output`
    // stays a hidden back-compat alias for existing scripts.
    #[arg(long = "out", alias = "output", value_name = "FILE")]
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
    #[arg(value_parser = non_empty_value)]
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
    /// `--format json` and kept for back-compat; conflicts with an
    /// explicit `--format`.
    #[arg(long, conflicts_with = "format")]
    json: bool,
    /// 输出格式：`text`（默认）/ `json` / `mermaid`。`mermaid`
    /// 渲染 "changed files → impacted business → suggested tests" 的
    /// 局部 `flowchart LR`，边来自 `ImpactReport.impact_edges`
    /// （真实图边），不是合成近似关系。
    #[arg(long, value_enum, default_value_t = ImpactFormatArg::Text)]
    format: ImpactFormatArg,
    /// `--format mermaid` 的输出文件路径；省略时打印到 stdout。
    // `--out` is the canonical name across all commands (#91); `--output`
    // stays a hidden back-compat alias for existing scripts.
    #[arg(long = "out", alias = "output", value_name = "FILE")]
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
    #[arg(value_parser = non_empty_value)]
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

/// clap `value_parser` that rejects an empty / whitespace-only positional
/// argument (#114). An empty id or query is never meaningful and otherwise
/// reaches the engine as a silent zero-hit query (or a downstream unwrap); a
/// usage error at parse time is the honest, scriptable response.
fn non_empty_value(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err("value must not be empty".to_string())
    } else {
        Ok(s.to_string())
    }
}

fn main() -> ExitCode {
    reset_sigpipe();
    match run() {
        Ok(code) => ExitCode::from(code),
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

fn run() -> Result<u8> {
    let cli = Cli::parse();
    let cmd_name = command_name(&cli.command);
    let repo_root = cli.repo_root.clone();
    let start = std::time::Instant::now();
    let outcome: Result<u8> = dispatch(cli);
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let ok = matches!(&outcome, Ok(0));
    let metrics = specslice_engine::stats::take_metrics();
    let stat = specslice_engine::stats::make_stat(cmd_name, duration_ms, ok, metrics);
    let _ = specslice_engine::stats::append_stat(&repo_root.join(".specslice"), &stat);
    outcome
}

/// Clamp a runner's `i32` exit code into the `u8` the process returns. Negative
/// or oversized codes (none are expected — they are 0/1/2) collapse to 1.
fn exit_code(exit: i32) -> u8 {
    u8::try_from(exit).unwrap_or(1)
}

/// Stable kebab-case name for a command, used as the stats ledger key.
fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Init => "init",
        Commands::Index(_) => "index",
        Commands::Slice(_) => "slice",
        Commands::Impact(_) => "impact",
        Commands::Check(_) => "check",
        Commands::Context(_) => "context",
        Commands::Connect(args) => match &args.sub {
            ConnectSub::Propose(_) => "connect-propose",
            ConnectSub::Apply(_) => "connect-apply",
        },
        Commands::Export(_) => "export",
        Commands::Graph(_) => "graph",
        Commands::Candidate(args) => match &args.sub {
            CandidateSub::List(_) => "candidate-list",
            CandidateSub::Show(_) => "candidate-show",
            CandidateSub::Review(_) => "candidate-review",
        },
        Commands::Logic(_) => "logic",
        Commands::Propose(_) => "propose",
        Commands::BusinessDoc(_) => "business-doc",
        Commands::Search(_) => "search",
        Commands::DeadCode(_) => "dead-code",
        Commands::Similar(_) => "similar",
        Commands::SelectTests(_) => "select-tests",
        Commands::Features(_) => "features",
        Commands::GraphDiff(_) => "graph-diff",
        Commands::Questions(_) => "questions",
        Commands::Dashboard(_) => "dashboard",
        Commands::Facts(_) => "facts",
        Commands::Purity(_) => "purity",
        Commands::Constants(_) => "constants",
        Commands::Contract(_) => "contract",
        Commands::PortCoverage(_) => "port-coverage",
        Commands::RouteCoverage(_) => "route-coverage",
        Commands::GraphEquiv(_) => "graph-equiv",
        Commands::SchemaIndex(_) => "schema-index",
        Commands::SuggestTests(_) => "suggest-tests",
        Commands::FeaturePack(_) => "feature-pack",
        Commands::Stats(_) => "stats",
        Commands::Trace(_) => "trace",
    }
}

/// Dispatch a parsed command to its runner, returning the process exit code.
/// Takes the whole `Cli` by value: the match consumes `cli.command` while the
/// arms read the disjoint `cli.repo_root` field (allowed within one function).
fn dispatch(cli: Cli) -> Result<u8> {
    match cli.command {
        Commands::Init => commands::init::run(&cli.repo_root).map(|()| 0),
        Commands::Index(args) => commands::index::run(&cli.repo_root, args.docs_only).map(|()| 0),
        Commands::Slice(args) => commands::slice::run(
            &cli.repo_root,
            &args.requirement,
            args.json,
            args.call_depth,
        )
        .map(|()| 0),
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
            commands::impact::run(&cli.repo_root, &args.base, head, format, args.output).map(|()| 0)
        }
        Commands::Check(args) => {
            let exit = commands::check::run(&cli.repo_root, args.json, args.fail_on_warning)?;
            Ok(exit_code(exit))
        }
        Commands::Context(args) => commands::context::run(
            &cli.repo_root,
            &args.requirement,
            !args.no_snippets,
            args.json,
        )
        .map(|()| 0),
        Commands::Connect(args) => match args.sub {
            ConnectSub::Propose(p) => {
                commands::connect::run_propose(&cli.repo_root, p.out.as_deref(), p.pretty)
                    .map(|()| 0)
            }
            ConnectSub::Apply(a) => {
                let exit =
                    commands::connect::run_apply(&cli.repo_root, a.candidates, a.dry_run, a.json)?;
                Ok(exit_code(exit))
            }
        },
        Commands::Export(args) => {
            commands::export::run(&cli.repo_root, args.format.into()).map(|()| 0)
        }
        Commands::Candidate(args) => match args.sub {
            CandidateSub::List(a) => {
                let mode = if a.all {
                    commands::candidate::ListMode::All
                } else {
                    commands::candidate::ListMode::Pending
                };
                commands::candidate::run_list(&cli.repo_root, mode, a.json).map(|()| 0)
            }
            CandidateSub::Show(a) => {
                let format = if a.json {
                    commands::candidate::CandidateShowFormat::Json
                } else {
                    a.format.into_command_format()
                };
                let exit = commands::candidate::run_show(&cli.repo_root, &a.id, format)?;
                Ok(exit_code(exit))
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
                Ok(exit_code(exit))
            }
        },
        Commands::Logic(args) => {
            let exit = commands::logic::run(&cli.repo_root, args.json, args.only_risks)?;
            Ok(exit_code(exit))
        }
        Commands::Propose(args) => commands::propose::run(commands::propose::ProposeRunArgs {
            repo_root: cli.repo_root.clone(),
            format: args.format.into_command_format(),
            out: args.out,
            pretty: args.pretty,
            max_modules: args.max_modules,
            max_entry_points: args.max_entry_points,
        })
        .map(|()| 0),
        Commands::BusinessDoc(args) => {
            commands::business_doc::run(commands::business_doc::BusinessDocRunArgs {
                repo_root: cli.repo_root.clone(),
                format: args.format.into_command_format(),
                out: args.out,
                include_proposed: args.include_proposed,
                include_rejected: args.include_rejected,
                pretty: args.pretty,
            })
            .map(|()| 0)
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
        })
        .map(|()| 0),
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
            .map(|()| 0)
        }
        Commands::DeadCode(args) => {
            commands::dead_code::run(commands::dead_code::DeadCodeRunArgs {
                repo_root: cli.repo_root.clone(),
                min_confidence: args.min_confidence.into_engine(),
                include_tests: args.include_tests,
                json: args.json,
            })
            .map(|()| 0)
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
        })
        .map(|()| 0),
        Commands::SelectTests(args) => {
            // Empty head ref diffs `--base` against the working tree, mirroring
            // `impact --worktree` so the two git-diff analyses behave the same.
            let head = if args.worktree {
                String::new()
            } else {
                args.head
            };
            commands::select_tests::run(commands::select_tests::SelectTestsRunArgs {
                repo_root: cli.repo_root.clone(),
                base_ref: args.base,
                head_ref: head,
                include_dependent: args.include_deps,
                max_propagation_depth: args.max_depth,
                format: args.format,
            })
            .map(|()| 0)
        }
        Commands::Features(args) => commands::features::run(commands::features::FeaturesRunArgs {
            repo_root: cli.repo_root.clone(),
            max_clusters: args.max_clusters,
            max_propagation_depth: args.max_depth,
            min_cluster_size: args.min_cluster_size,
            format: args.format,
        })
        .map(|()| 0),
        Commands::GraphDiff(args) => {
            commands::graph_diff::run(commands::graph_diff::GraphDiffRunArgs {
                base_db: args.base_db,
                head_db: args.head_db,
                base_repo_root: args.base_repo_root,
                head_repo_root: args.head_repo_root,
                format: args.format,
            })
            .map(|()| 0)
        }
        Commands::Questions(args) => {
            commands::questions::run(commands::questions::QuestionsRunArgs {
                repo_root: cli.repo_root.clone(),
                max_per_category: args.max_per_category,
                format: args.format,
            })
            .map(|()| 0)
        }
        Commands::Dashboard(args) => {
            commands::dashboard::run(commands::dashboard::DashboardRunArgs {
                repo_root: cli.repo_root.clone(),
                out: args.out,
            })
            .map(|()| 0)
        }
        Commands::Facts(args) => {
            let purity = match args.purity {
                Some(s) => Some(commands::facts::parse_purity(&s)?),
                None => None,
            };
            commands::facts::run_facts(commands::facts::FactsRunArgs {
                repo_root: cli.repo_root.clone(),
                include_types: args.include_types,
                purity,
                max: args.max,
                max_evidence: args.max_evidence,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::Purity(args) => {
            let only = match args.only {
                Some(s) => Some(commands::facts::parse_purity(&s)?),
                None => None,
            };
            commands::facts::run_purity(commands::facts::PurityRunArgs {
                repo_root: cli.repo_root.clone(),
                include_types: args.include_types,
                only,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::Constants(args) => {
            let kind = match args.kind {
                Some(s) => Some(commands::constants::parse_kind(&s)?),
                None => None,
            };
            commands::constants::run(commands::constants::ConstantsRunArgs {
                repo_root: cli.repo_root.clone(),
                include_types: args.include_types,
                include_trivial: args.include_trivial,
                min_occurrences: args.min_occurrences,
                kind,
                max: args.max,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::Contract(args) => commands::contract::run(commands::contract::ContractRunArgs {
            repo_root: cli.repo_root.clone(),
            tables_only: args.tables_only,
            keys_only: args.keys_only,
            json: args.json,
        })
        .map(|()| 0),
        Commands::PortCoverage(args) => {
            commands::port_coverage::run(commands::port_coverage::PortCoverageRunArgs {
                source_db: args.source_db,
                target_db: args.target_db,
                include_types: !args.callables_only,
                include_extra: args.include_extra,
                include_generated: args.include_generated,
                include_tests: args.include_tests,
                include_synthetic: args.include_synthetic,
                normalize_names: args.normalize_names,
                ignore_case: args.ignore_case,
                port_map: args.port_map,
                exclude: args.exclude,
                source_include: args.source_include,
                source_exclude: args.source_exclude,
                max: args.max,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::RouteCoverage(args) => {
            commands::route_coverage::run(commands::route_coverage::RouteCoverageRunArgs {
                source_db: args.source_db,
                target_db: args.target_db,
                suffix_segments: args.suffix_segments,
                include_extra: args.include_extra,
                exclude: args.exclude,
                max: args.max,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::GraphEquiv(args) => {
            commands::graph_equiv::run(commands::graph_equiv::GraphEquivRunArgs {
                source_db: args.source_db,
                target_db: args.target_db,
                source_scope: args.source_scope,
                target_scope: args.target_scope,
                callables_only: args.callables_only,
                ignore_case: args.ignore_case,
                normalize_names: args.normalize_names,
                include_generated: args.include_generated,
                include_tests: args.include_tests,
                max: args.max,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::SchemaIndex(args) => {
            commands::schema_index::run(commands::schema_index::SchemaIndexRunArgs {
                repo_root: cli.repo_root.clone(),
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::SuggestTests(args) => {
            commands::suggest_tests::run(commands::suggest_tests::SuggestTestsRunArgs {
                repo_root: cli.repo_root.clone(),
                include_types: args.include_types,
                only_pure: args.only_pure,
                min_priority: args.min_priority,
                max: args.max,
                json: args.json,
            })
            .map(|()| 0)
        }
        Commands::FeaturePack(args) => {
            commands::feature_pack::run(commands::feature_pack::FeaturePackRunArgs {
                repo_root: cli.repo_root.clone(),
                path: args.path,
                requirement: args.requirement,
                max_evidence: args.max_evidence,
                text: args.text,
            })
            .map(|()| 0)
        }
        Commands::Stats(args) => commands::stats::run(commands::stats::StatsRunArgs {
            repo_root: cli.repo_root.clone(),
            json: args.json,
            reset: args.reset,
        })
        .map(|()| 0),
        Commands::Trace(args) => commands::trace::run(commands::trace::TraceRunArgs {
            repo_root: cli.repo_root.clone(),
            query: args.query,
            max_depth: args.depth,
            max_nodes: args.max_nodes,
            max_seeds: args.seeds,
            include_noise: args.include_noise,
            json: args.json,
        })
        .map(|()| 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// issues2.md #58: `--json` is a legacy alias for `--format json`.
    /// Passing both an explicit `--format` and `--json` used to silently
    /// pick JSON; it must be a hard parse error instead.
    #[test]
    fn json_flag_conflicts_with_explicit_format() {
        for argv in [
            vec!["specslice", "search", "foo", "--format", "html", "--json"],
            vec!["specslice", "impact", "--format", "mermaid", "--json"],
            vec![
                "specslice",
                "candidate",
                "show",
                "x",
                "--format",
                "text",
                "--json",
            ],
        ] {
            let err = Cli::try_parse_from(&argv)
                .expect_err(&format!("{argv:?} must be rejected as conflicting"));
            assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    /// Each flag alone keeps working — back-compat for existing scripts.
    #[test]
    fn json_flag_alone_and_format_alone_still_parse() {
        assert!(Cli::try_parse_from(["specslice", "search", "foo", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["specslice", "search", "foo", "--format", "html"]).is_ok());
        assert!(Cli::try_parse_from(["specslice", "impact", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["specslice", "candidate", "show", "x", "--json"]).is_ok());
    }

    /// #114: an empty / whitespace-only positional id or query must be a parse
    /// error (clap usage, exit 2), not forwarded to the engine as a zero-hit
    /// query or a downstream unwrap.
    #[test]
    fn empty_positional_arguments_are_rejected() {
        for argv in [
            vec!["specslice", "trace", ""],
            vec!["specslice", "slice", ""],
            vec!["specslice", "context", ""],
            vec!["specslice", "candidate", "show", ""],
            vec!["specslice", "candidate", "review", "", "--accept"],
            vec!["specslice", "trace", "   "],
        ] {
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "{argv:?}: empty positional must be rejected"
            );
        }
        // Non-empty values still parse.
        assert!(Cli::try_parse_from(["specslice", "trace", "selectFoo"]).is_ok());
        assert!(Cli::try_parse_from(["specslice", "slice", "REQ-1"]).is_ok());
        assert!(Cli::try_parse_from(["specslice", "candidate", "show", "c1"]).is_ok());
    }

    /// #91: the file-output flag is `--out` on every command; `--output`
    /// remains a back-compat alias on the two commands that historically used
    /// it (`search`, `impact`).
    #[test]
    fn out_flag_is_unified_with_output_alias() {
        assert!(Cli::try_parse_from(["specslice", "graph", "--out", "g.json"]).is_ok());
        for flag in ["--out", "--output"] {
            assert!(
                Cli::try_parse_from(["specslice", "search", "q", flag, "s.html"]).is_ok(),
                "search must accept {flag}"
            );
            assert!(
                Cli::try_parse_from(["specslice", "impact", flag, "i.json"]).is_ok(),
                "impact must accept {flag}"
            );
        }
    }

    /// #112: `select-tests` and `impact` are both git-diff analyses; their
    /// default base ref must be unified so switching commands is not a trap.
    #[test]
    fn select_tests_and_impact_share_default_base_ref() {
        let select_base = match Cli::try_parse_from(["specslice", "select-tests"])
            .unwrap()
            .command
        {
            Commands::SelectTests(a) => a.base,
            other => panic!("expected select-tests, got {other:?}"),
        };
        let impact_base = match Cli::try_parse_from(["specslice", "impact"])
            .unwrap()
            .command
        {
            Commands::Impact(a) => a.base,
            other => panic!("expected impact, got {other:?}"),
        };
        assert_eq!(select_base, "origin/main");
        assert_eq!(select_base, impact_base, "default base ref must be unified");
    }

    /// `impact` and `select-tests` are both git-diff analyses; `impact` grew a
    /// `--worktree` flag to run on uncommitted edits, so `select-tests` must
    /// accept it too — otherwise switching between the two is a trap (#112).
    #[test]
    fn select_tests_accepts_worktree_like_impact() {
        assert!(
            Cli::try_parse_from(["specslice", "select-tests", "--base", "HEAD", "--worktree"])
                .is_ok(),
            "select-tests must accept --worktree for parity with impact"
        );
        let worktree = match Cli::try_parse_from(["specslice", "select-tests", "--worktree"])
            .unwrap()
            .command
        {
            Commands::SelectTests(a) => a.worktree,
            other => panic!("expected select-tests, got {other:?}"),
        };
        assert!(worktree, "--worktree must set the worktree flag");
    }
}
