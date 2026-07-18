# Changelog

## [0.3.1] - 2026-07-18

### 修复

- **子进程超时死锁**（#271，High）：`index` 在子进程超时杀树后不再无条件 join 管道读取线程——逃逸进程组的孙进程（dartdev / analysis_server 类守护进程）持有管道不关，会导致 `groundgraph index` 在 Linux 上永久挂起而非按预算降级。同时修复 `dart_sidecar` 与 `scip_runner` 两处同模式路径。
- CI：lint-and-test 任务安装 Dart SDK，Dart 金标测试在双平台真实运行（修复 main 自 2026-07-08 起的既有红）。

## [0.3.0] - 2026-07-18

### 预编译发布（头条）

- **GitHub Releases 预编译二进制**：打 `v*` tag 自动构建四个平台产物，用户免 Rust 工具链——macOS universal（aarch64+x86_64 lipo）、Linux x86_64/aarch64（musl 静态）、Windows x86_64（MSVC zip）。每包含 `groundgraph` + `groundgraph-mcp` 双二进制、Dart analyzer sidecar 源码、`skills/groundgraph`、校验和。macOS 包保留 Developer ID 签名/公证钩子（secrets 就位即启用）。
- `cargo package`/`cargo publish` 修复：webui 资产内化进 `groundgraph-cli` 包（`include_str!` 不再引用 crate 外文件），crates.io 发布链打通。

### 正确性与数据契约（破坏变更）

- **schema 迁移 005**（#151/#152/#188/#190）：`nodes`/`edge_assertions` 删除从未使用的 `source_hash`/`index_generation` 列，删除死表 `slice_cache`；旧库自动迁移、数据保留。
- **边身份契约**（#205）：边 ID 改为 `edge::{kind}::{source}::{len}:{from}::{to}`——同 `(kind,from,to)` 不同来源的边不再互相覆盖；UPSERT 增加 certainty 防降级。旧格式 ID 在下次 `groundgraph index` 时自动清理。
- **`Confidence` newtype**（#168/#63 根治）：confidence 由类型保证有限且 ∈[0,1]（NaN 无法表示），serde 线格式不变（裸数字）；手改 `candidates.yaml` 的越界/`.nan` 值在加载时自动净化。
- **`Node` 写入边界校验**（#168）：`start_line > end_line` 的节点在 store 写入时被拒绝（`StoreError::InvalidNode`）。
- **退出码契约**（#233/#115/#232）：0 成功 / 2 用户错误（参数非法、无工作区、未找到、check 发现 error、index 部分失败）/ 70 内部错误；7 处 `--format` 改 ValueEnum；`index` 新增 `--fail-on-partial`。**脚本依赖旧"几乎全 1"行为的需要适配。**
- **路径安全**（#145/#242/#263）：22 份 storage 路径解析副本收敛为单一 `confine_under_root`，engine 侧统一获得 `..` 逃逸防护。

### 新功能

- `groundgraph doctor`：环境诊断（git/SCIP/LSP/Dart/graph.db/配置），带可操作建议（#116）。
- `groundgraph completions <shell>`：bash/zsh/fish/powershell/elvish 补全脚本（#113）。
- 可观测性（#127/#230/#231/#234）：tracing 日志（`-v`/`-vv`/`-q`/`RUST_LOG`）、`index` 进度指示（TTY spinner / 非 TTY 静默）、`--help` 新增 Environment 段 + `docs/environment.md`。
- webui 中英双语：`?lang=`/localStorage/浏览器语言三级解析，右上角切换（#175）。
- 解析器：Rust `macro_rules!` 节点 + 宏调用边（#123）；C# LINQ 调用捕获与 partial class 跨文件合并（#125）；新增 csharp/ruby/php/kotlin/cpp/c 六语言测试 fixture（#238）。
- CLI `--help`：36 个子命令分组（Setup/Query/Graph/…）+ 高频命令 Examples（#128）。

### 性能

- search 热路径（#143/#144/#156）：评分并行化 + token 借用 + lowercase 缓存，8000 节点基准下 -16%～-44%（criterion 基准随仓：`cargo bench`）。
- ingest（#137）：孤儿清理从每 indexer 一次全表扫收敛为整个 ingest 一次。
- 其他：requirement evidence 批查（#158）、`ScannedRef` Arc<str>（#160）、`explain_symbol` JSON 单遍分桶（#162）。

### 基础设施与供应链

- rusqlite 0.32→0.40（#213，内嵌 SQLite 同步升级）。
- SCIP 反序列化 protobuf→prost（#229），构建免系统 protoc。
- RUSTSEC 清零（4 项）：anyhow 1.0.103、crossbeam-epoch 0.9.20、serde_yml→serde_norway（#70 的替代品自身也不维护，二次迁移）、indicatif 0.18（number_prefix）。`cargo deny check` 全绿零豁免。
- engine 公共入口改为 typed `EngineError`（#166）；12 语言 adapter 调用收集骨架去重（#130）；子进程瞬时失败指数退避重试（#217，`GROUNDGRAPH_SUBPROCESS_RETRY_*` 可调）。
- 测试基建：golden 脚手架共享（#236）、单测覆盖棘轮入 CI（#221）、命名规范入 CONTRIBUTING（#239）。

### 升级注意

- `.groundgraph/graph.db` 打开时自动应用迁移 005（列删除、死表删除），无需手工干预；边 ID 新格式在下次 `index` 后完全生效。
- 外部脚本若解析 CLI 退出码，请按新契约（0/2/70）调整。
