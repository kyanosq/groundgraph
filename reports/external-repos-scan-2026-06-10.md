# 外部仓库扫描报告（2026-06-10）

> **注意**：本报告包含本机私有项目信息，仅供操作者本人使用，**请勿提交到开源仓库**。
>
> 扫描方式：将 `~/Code/My`、`~/Code/Features` 下的仓库 rsync 到 `/tmp/scan/<name>`
> （排除 build 产物与数据目录），依次运行 `groundgraph init → index → check →
> dead-code → similar`。所有发现都已在**原仓库**中逐条核实（文件存在性 / 符号
> grep），非真实的项目均已作为 GroundGraph 误报修复（见文末）。

## 扫描范围

16 个仓库，覆盖 Swift / SwiftUI、Dart / Flutter、Rust、Python、TypeScript/JS、
Go（Shift backend）、SPM、Xcode 工程与 monorepo 形态。全部 `init / index /
check / dead-code / similar` 均 exit 0，无 panic、无超时。

| 仓库 | 形态 | check 发现 | 备注 |
|---|---|---|---|
| Cleaner | Xcode/Swift | 0 | 干净 |
| sokoban | Rust | 0 | 暴露我方 benches 入口点误报（已修） |
| GlowMirror | Xcode/Swift | 0 | dead-code 76 medium 候选 |
| periodic_table | Flutter | 1 | 真实漂移 |
| AudioTool | SPM/Swift | 0 | 干净 |
| Zipper | Xcode+SPM | 0 | 暴露我方 `archive.h` 系统头误报（已修） |
| invis | Xcode/Swift | 0 | 干净 |
| GlobalGo | Xcode/Swift | 0 | 干净 |
| Notate | Xcode/Swift | 0 | 两条告警经 FTS 调用点兜底确认为非漂移 |
| turing | Flutter | 4 | 真实漂移 |
| hama | Flutter | 1 | 真实漂移 |
| ZenLock | Xcode/Swift | 3 | 真实漂移（疑似文档复制残留） |
| MarkIt | Xcode/Swift | 102 | 历史评审文档描述旧架构（整体漂移） |
| MetaQuant | Python+TS monorepo | 5 | 真实漂移 + dead-code 472 medium |
| nest | Node+Rust | 8 | 真实漂移 |
| Shift | Flutter+TS+Go+Swift monorepo | 4 | 真实漂移；72s 索引压力测试通过 |

## 各仓库发现明细（已核实）

### periodic_table（1）
- `docs/BUGFIXES_2025_11_04.md` 声称“新建: `lib/services/screenshot_service.dart`”，
  该文件不存在于 `lib/services/`。修 bug 记录与实际交付不符，建议核对该次
  修复是否真正落地。

### turing（4）
- `lib/services/expert_toolkit.dart`、`expert_toolkit_lock_badge.dart` —— 文档
  描述的“专家工具箱”模块在代码中不存在（功能未实现或已移除，文档未同步）。
- `test/game/macro_module_component_test.dart` —— 文档引用的测试文件不存在。

### hama（1）
- `current_project_lifecycle_service.dart` —— 文档引用的生命周期服务文件不存在
  （`lib/` 全树验证）。

### ZenLock（3）
- 文档引用 `Store.swift`、`ActivityManager.swift`、`InvisApp.swift`，三个文件
  在仓库任何位置都不存在。`InvisApp.swift` 是 **invis 项目**的入口文件名，
  高度疑似文档从 invis 复制后残留，建议清理文档。

### MarkIt（102 — 同一根因）
- `docs/exec-plans/ai-core-logic-review-bundle.md` 与 `ai-review-bundle.md`
  两份评审包描述的目录架构是 `MarkIt/Core|Views|Models|Services|Extensions|
  ViewModels|Protocols/...`（102 个文件路径），而仓库实际结构已重组为
  `MarkIt/App|Features|DesignSystem|Models|Redaction|Services/...`。
  历史评审文档整体过期。建议二选一：
  1. 在 MarkIt 配置 `.groundgraph.yaml → checks.doc_drift_ignore:
     ["docs/exec-plans/**"]`（按历史档案豁免）；
  2. 或为文档补一行“目录结构系当时快照”的免责说明。

### MetaQuant（5 + dead-code 概况）
- `apps/admin/e2e/research-oms.spec.ts` —— 文档引用的 e2e 测试不存在。
- `factor_fields.py`、`test_strategy.py`、`test_bug_reproduction.py` —— 文档
  引用的文件全仓不存在（曾存在后被删，或从未交付）。
- `tests/integration_tests/conftest.py` —— 引用路径不存在。
- dead-code：`src/metaquant` 内 472 个 medium 候选（探索性因子/策略函数居多，
  符合研究型仓库特征；其中 `_econ_*` 系列 API 包装函数完全无调用方，可作清理
  起点）。
- 另：`docs/references/rqalpha-source/`（第三方框架源码副本）此前会被当成
  第一方代码索引，本次已在 GroundGraph 侧修复（探测排除 vendored 目录）。

### nest（8）
- `references/spike.md`、`references/critical-overrides.md`、
  `references/communication-protocol.md` —— 文档互引的三份笔记不存在。
- `output/eval/report.json` —— 文档承诺的评估产物不存在（流水线未跑或产物
  未入库）。
- `BoardNestingV2.jsx`、`Topbar.tsx`、`ParamsPanel.tsx` —— 文档描述的组件
  文件已不存在（前端重构后文档未更新）。
- `migrate_mode_to_recipe()` —— 文档描述的迁移函数在代码与全文索引中均无。

### Shift（4）
- 文档引用 `routes.go`、`webhook.go`、`feedback_service.go` —— backend 实际
  不含这三个文件（Go 后端设计文档与实现漂移，或后端已换形态）。
- `parseRSAPublicKey()` —— 文档描述的密钥解析函数在代码中不存在。

### dead-code 概况（供清理参考，均为 medium 置信度报告，不建议盲删）
- Cleaner 29 / GlowMirror 76 / sokoban 4 / MetaQuant 472。
- Swift 仓库的候选集中在 View 扩展与运算符重载（外部消费者可能使用，故仅
  medium）；MetaQuant 的候选集中在研究性因子函数。

## 本次扫描反向驱动的 GroundGraph 修复（已全部落地 + 测试）

| 来源仓库 | 暴露问题 | 修复 |
|---|---|---|
| sokoban | `benches/` 函数被报死代码 | Rust `benches/**`、`examples/**`、`build.rs` 默认视为 cargo 入口点 |
| Zipper | `archive.h` 系统头文件被报漂移 | 裸 `.h/.hpp` 文件名不再验证（路径形式仍验证） |
| MetaQuant | `../README.md` 相对路径误报 | `../` 开头的引用跳过（相对文档自身，不可用仓库根验证） |
| MetaQuant | `round-XX-report-YYYY-MM-DD.md` 模板名误报 | `XX/YYYY/MM/DD/HH/NN` 模板 token 检测 |
| nest / hama | `click()`/`pop()`/`compute()` 平台 API 误报 | 裸调用仅验证多词标识符（`snake_case`/`camelCase` ≥2 词） |
| hama / Notate / ZenLock | `computeLuminance()` 等有调用点无定义节点的 API 误报 | FTS 调用点兜底：代码体内出现过该标识符即不算漂移（排除文档自证） |
| MetaQuant | `docs/references/` 下第三方源码副本被索引为第一方代码 | init 语言探测跳过 `docs/doc/vendor/third_party/thirdparty/external/references` |
| （自身 dogfood） | `tool/walker.dart` 未索引目录文件误报 | 工作树 basename 回退 |
| （路线图） | agent 无法直接取用漂移检查 | 新增 MCP 工具 `check_drift`（第 7 个工具） |

修复后全部 16 仓复扫：剩余告警均为上文列出的**已核实真实漂移**；
GroundGraph 自身仓库 `check` 保持 0 findings（灵敏度对照实验确认检查未失灵）。
