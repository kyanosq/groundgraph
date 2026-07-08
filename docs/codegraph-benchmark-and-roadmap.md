# GroundGraph ✕ CodeGraph 对标分析与演进规划

> 文档目的：把当前项目（GroundGraph）与参考标杆 `docs/sourcecode/codegraph`（colbymchenry/codegraph，v0.9.7）放在同一坐标系里，澄清二者的真实定位差异、可借鉴点、自身护城河，并给出可执行的演进路线和命名建议，作为后续迭代的对齐基准。
>
> 调研基准：GroundGraph 仓库 crates 实际代码（约 49k 行 Rust，6 crate）、`PRD.md`、`docs/implementation-plan.md`（已落地到 v0.3.0-A + P21/P22 tree-sitter 广度后端）；CodeGraph 的 `README.md`、`CLAUDE.md`、`CHANGELOG.md`（v0.9.7）、`src/` 结构（118 个 TS 文件）。

---

## 0. 一句话结论

**二者不是同一个产品，是相邻产品。** CodeGraph 是「给 AI agent 的高性能代码检索层」（一个更好的 grep / Explore 替代品，刻意不碰产品需求）；GroundGraph 是「给 AI 编程的代码库**意图治理层**」（把需求/文档/测试与代码用证据连起来，AI 提候选、人确认）。

- **错位竞争是 GroundGraph 的机会**：CodeGraph 在 README/CLAUDE.md 里明确写 *"CodeGraph provides code context, not product requirements"* —— 它主动让出的正是 GroundGraph 的主战场（意图对齐 / 可追溯 / Doc-Code 漂移）。
- **风险点**：GroundGraph 底层那部分「纯代码图」能力（search / calls / impact-on-code / 多语言）与 CodeGraph 高度重叠，但成熟度落后（6 语言 vs 20+、手动 index vs 自动 auto-sync、无 trace / 动态分发合成）。若在「纯代码检索」上跟 CodeGraph 正面拼，会输；正确做法是**借它的检索工程学打底，把精力压到意图层护城河上**。

---

## 0.5 实测对标（2026-05-31，两个二进制真实互索引）

> 方法：用 `groundgraph` release 二进制与 `codegraph` v0.9.7 dist，对**同一批目标**做非侵入索引（目标仓先 rsync 到 `/tmp` 副本，剔除构建产物，0 写回源仓）。Morse=Flutter/Dart 真实仓，Panelly=Swift/iOS 真实仓（用户口中的 "Penlly" 实为 `~/Code/My/Panelly`，已据此校准）。

| 目标 | 工具 | 文件 | 符号/节点 | 边 | 解析器 | 耗时 |
|---|---|---|---|---|---|---|
| Morse (Dart) | **GroundGraph** | 320 dart | 13 529 符号 + 981 测试 | — | `dart_analyzer`（LSP sidecar 命中） | **22.9s** |
| Morse (Dart) | CodeGraph | 388（含 tsx/ts/yaml/js/py 全语言） | 15 404 节点 | 25 136 | tree-sitter | **5.6s** |
| Panelly (Swift) | **GroundGraph** | 105 swift | 1 191 符号 | — | `swift_lsp`（sourcekit-lsp，**调用链超时降级**） | **~80s（含 LSP 超时/断管）** |
| Panelly (Swift) | CodeGraph | 259（含 151 cpp + 1 c + swift） | 3 652 节点 | 9 982 | tree-sitter | **1.6s** |
| GroundGraph 自身 (Rust) | **GroundGraph**（P21 前） | — | ❌ 不支持 Rust，无法自举 | — | — | — |
| GroundGraph 自身 (Rust) | **GroundGraph**（P21 后） | 125 rust | 2 023 符号 + 748 imports | file→symbol/contains | `rust_treesitter`（**进程内 tree-sitter**） | ~8s（含二进制+docs，rust pass 占小头） |
| GroundGraph 自身 (Rust) | CodeGraph | 122 rust | 3 009 节点 | 9 434 | tree-sitter | **0.48s** |
| CodeGraph 自身 (TS) | **GroundGraph** | 116 ts | 3 652 符号 | — | `typescript_lsp` | 7.7s |

**从真实数据得到的硬结论：**

1. **索引速度差一个数量级，且来自架构。** CodeGraph 的 tree-sitter 在进程内、零外部依赖，Rust 122 文件 0.48s、Swift 1.6s；GroundGraph 的 LSP sidecar 路线 Dart 22.9s、Swift ~80s 且 sourcekit-lsp 在大仓上 **callHierarchy/references 超时 + Broken pipe**，调用链大面积降级。**纯索引吞吐不是 GroundGraph 的战场**（再次印证 §0）。
2. **覆盖广度差距正在收窄（自举缺口已堵，Tier 2 已推广到 9 语言且全产调用图）。** CodeGraph 一次吃下目标仓里所有语言（Morse 里连 tsx/py、Panelly 里连 C/C++ 依赖都进图）；GroundGraph 原先只索引配置启用的单语言。~~且不能索引自身（Rust 无适配器，连 dogfood 都做不到）~~ → **P21 落地 Rust tree-sitter 后端、GroundGraph 能自举（125 文件 / 2023 符号）；P22 进一步把进程内 tree-sitter 收敛成通用数据驱动驱动（`LangSpec`），把 Tier 2 推广到 Rust / TypeScript / Python / Go / C / C++；P23 再把 medium 置信启发式调用解析补齐到全部 9 门 tree-sitter 语言（结构 + import + `Calls`/`References`），单个 `treesitter:`/`languages:` 配置开关即可多语言索引**。广度差距仍在（CodeGraph 20+ 且含动态分发/跨语言桥），但「语言太少 + 不能自举 + 依赖外部 LSP + 太慢 + 纯 tree-sitter 语言无调用图」五个实测短板已由 Tier 2 + 启发式调用解析直接解掉。
3. **GroundGraph 的价值不在"图多大/多快"，而在"图上挂了什么"**：`dart_analyzer` 解析出 `dart_provider` 等语义类型、dead-code 带中文理由与置信度、search 带 evidence 与邻接原因——这些 CodeGraph 的结构图里没有。

**查询体验对比（Morse 上同题）：**
- CodeGraph `context "agent provider state"`：直接吐出 Entry Points + Related Symbols + **内联代码片段** 的 markdown，358ms，agent 拿来即用。
- GroundGraph `search provider`：返回 `dart_provider` 节点 + 分数 + `来源:dart_analyzer` + 邻接命中原因，**语义/证据更丰富但更"工具化"**，且 `context_pack` 才给代码片段。→ 印证 §3.1「检索充分性」要补。

---

## 0.6 可行性结论：能否「替代并增强 CodeGraph」？

**结论：不要做"替代"，要做"覆盖 + 错位增强"——技术上可行，正面替代不划算。**

- **"完全替代"不可行也不值得**：CodeGraph 的护城河是 tree-sitter 广度 + 进程内速度 + auto-sync + 一行安装 + 8 agent 自动配置，已 v0.9.7。GroundGraph 要在「纯代码检索」追平这些，等于用 LSP 路线去拼 tree-sitter 的主场——实测已证明速度/广度全面落后，投入产出极差。
- **"功能覆盖 + 上层增强"可行**：把 CodeGraph 的能力当作 GroundGraph 的 **Tier 2 广度后端**（§5.1），即「**内化它的检索层、对外讲意图对齐层**」。路径：
  1. 引入 tree-sitter 作为广度/兜底后端 → 一举解决「语言太少 + 不能自举 + 依赖外部 LSP + 太慢」四个实测短板；LSP 仅保留给需要高可信调用链的语言。（**已落地：P21 起步 Rust tree-sitter 后端 + 自举；P22 把它收敛成通用 `LangSpec` 数据驱动驱动并推广到 Rust/TS/Python/Go/C/C++ 六语言，逐语言配 proptest「任意输入不 panic + 确定性 + 良构」+ 端到端多语言集成测试；P23 把 medium 置信启发式调用解析补齐到全部 9 门 tree-sitter 语言（Java/Swift 含在内），9 语言均产 `Calls`/`References`。下一步是把 Tier 2 再补 C#/Ruby/Kotlin 等，并按需给高可信语言叠 Tier 3 LSP。**）
  2. 在统一图之上叠 GroundGraph 独有的证据层 / 候选-确认 / Doc-Code 漂移 / PR→需求影响（CodeGraph 结构上不做）。
  3. 检索体验（trace / 自适应 context / auto-sync / 一行安装）按 §6 Phase A·B 补齐到"够用"。
- **一句话**：GroundGraph 可以把 CodeGraph **做的事都做到（靠 tree-sitter 内化）**，并在其**明确放弃的意图层**上增强；但"替代"叙事会把自己拖回它的主场，应避免。

---

## 0.7 本轮借鉴 CodeGraph 落地的修复（已 TDD 验证）

实测中发现两处真实缺陷，已按「先写失败测试→最小实现→全绿」修复（581+ 测试全过，release 复验）：

1. **LSP 噪声爆 stdout（输出卫生）。** 索引 Panelly 时 sourcekit-lsp 超时/断管，`push_partial_warning` 把**每个符号的失败**用「；」无上限拼接，单行 **185 KB** 直冲 stdout——对管道/MCP 是灾难。修复：明细封顶 8 条 + 折叠计数（`…（另有 N 条 LSP 警告已折叠）`）。实测 Panelly 索引 stdout 从 **185 KB → 1 371 字节（−99.3%）**。对应 CodeGraph 的 *"partial coverage / 输出要克制"* 哲学（`crates/groundgraph-engine/src/lsp_indexer.rs`）。
2. **`search --kind` 拒收 TS/Java 类型（提示与实现不一致）。** `parse_kinds` 只支持 Dart/Swift/Go/Python，`--kind typescript_function` / `java_method` 直接报错，但报错信息里 `default_search_kinds()` 又把它们列为 "valid"。修复：补齐 TS（含 `ts_*` 短别名）/Java 全部 kind 别名（`crates/groundgraph-cli/src/commands/search.rs`）。

---

## 0.8 第二轮审计修复（2026-05-31 · search 性能 / 边索引 / impact 工作区 / embedding 决策）

把 GroundGraph 自身当审计工具二次 dogfood，又用 CodeGraph dist 实跑互证，落地以下修复（均「先写失败测试→最小实现→全绿」，workspace 全测试通过、`cargo fmt` 干净）：

1. **`search` 多词查询雪崩（230s → 0.06s）。** 实测 tailorx（28 159 节点 / 60 942 边）上 `search "craft tree"` 耗时 **230s**。两处真因：
   - **`edge_assertions` 缺邻接索引。** 001 schema 只有 `id` 主键，`list_edges_from/to`（`WHERE from_id/to_id = ?`）每次都是 6 万行全表扫描。搜索的 evidence/neighbor 加权对**每个命中**各扫一次，命中上千 → 数亿行读。→ 新增迁移 `002_edge_indexes.sql`（`idx_edge_assertions_from/to/kind` + `idx_evidence_artifact`，对标 CodeGraph 的 `idx_edges_source_kind/target_kind`）。
   - **加权遍历未先截断。** `apply_evidence_boost` / `apply_neighbor_boost` 在 `truncate(limit)` **之前**对整个未截断命中集逐条查库。→ 先按基础分排序，截到 `boost_window(limit)=max(limit,256)` 再加权。
   - 复合效果：`craft tree` 230s→**0.06s**，`dosingVolume` 9.6s→0.05s，与 CodeGraph 的 70ms 同档。dead-code（20 346 符号）0.48s。
2. **`search` 分数随 `--limit` 漂移（130↔150）。** 旧 `boost_window=limit*8` 让邻接加权的 `hit_ids` 集合随 limit 变化，同一节点在 `--limit 5` 得 130、`--limit 30` 得 150。→ 窗口改为**与 limit 无关的平地板** `max(limit,256)`，且邻接判定用**完整命中集**（只有逐条查库被窗口限流，成员判定不限流），分数恒定。
3. **`impact` 支持工作区 diff（`--worktree`）。** 旧 `impact` 只能 `git diff base..head`，要审未提交改动得先造一个丢弃 commit。→ `git_diff` 在 `head` 为空时走 `git diff <base>`（base vs 工作区）；CLI 加 `--worktree`。实测在 GroundGraph 自身未提交改动上：47 文件 / 295 符号 → 5 条需求 + 7 测试，并给出「受影响需求有测试但本次没有测试改动」漂移告警。
4. **`embedding` 决策：不引入核心检索/信任路径。** 详见 §5.3。要点：CodeGraph v0.9.7（成熟标杆）**零 embedding**——`schema.sql` 用 FTS5 + B 树索引、tree-sitter 确定性 AST；GroundGraph 的护城河信条本就是「确定性 / 证据 / 可进 CI」，向量近邻与之冲突。本轮证明 search 慢的真因是**缺索引**而非缺语义，修复后已达 CodeGraph 同档，**无需 embedding**。

> 备注：`search` / `slice` / `dead-code` / `context` 等只读命令打开 store 时不自动迁移，新增的邻接索引在**下次 `groundgraph index`** 时生效（已验证：用新二进制 `index` 后自身 db 自动建好 4 个索引）。catastrophic 的窗口修复是纯 Rust、无需迁移即时生效。

---

## 0.9 第三轮审计（2026-05-31 · 算法对比 / 双应用交叉扫描 / dead-code 校验与修复）

针对「算法谁更好、还有什么要处理、双应用扫描、dead-code 是否正常」四问，做了一轮以**真实命令输出为准**的评估，落地一处 TDD 修复，并定出下一步的头号工程项。

### 1) 算法选择对比：各擅其长，结论是「**借检索、守排序**」

| 维度 | GroundGraph | CodeGraph |
|---|---|---|
| 候选检索 | `list_all_nodes()` **全量内存线性扫描** + 分档打分 | **FTS5 倒排索引 + BM25**（`MATCH`，亚线性） |
| 分词/召回 | camel/snake 拆分 + compact | 同 + **词干变体**（caching→cache、eviction→evict）扩召回 |
| 排序模型 | 确定性分档（exact id 100 / name 80 / path 60 / token 50 / 弱子串 10）+ **图感知加权**（evidence +30、neighbor +20） | BM25 + `nameMatchBonus`/`kindBonus`/`pathRelevance` 启发式重排；**图不参与排序**（仅做上下文扩展） |
| 可解释/确定性 | 每条命中带 `match_reasons` + 证据；同查询稳定可复现 | 确定性（无 embedding），但 BM25 浮点分、无逐条证据 |
| 规模 | O(N) 扫描，靠 §0.8 窗口+边索引兜底；28k 节点 0.06s | O(log N) 倒排，137 文件 70ms |

**评估**：纯文本检索的**数据结构** CodeGraph 更优（倒排 vs 全扫、且有词干召回）；但**排序模型**为审计目的服务时 GroundGraph 更对路（图感知聚类 + 可逐条解释 + 证据，契合「确定性/可进 CI」信条）。规模上，§0.8 修复后 28k 节点已达 CodeGraph 同档，O(N) 暂未成为瓶颈。**行动**：把 FTS 当**候选生成器**、词干当**召回增强**借过来，排序仍走图感知打分——是「鱼与熊掌」式增量，列入路线但非当务之急（节点量级 ≪ 百万级前不紧迫）。

### 2) 双应用交叉扫描发现（CodeGraph dist 实跑互证）

- **CodeGraph 索引 GroundGraph（Rust）**：376 文件 → **6 814 节点 / 20 555 边 / 2.3s**；`query "boost window"` 0.4s、命中簇与 GroundGraph 一致；`callers run_search_with_store` 正确返回 `run_search` / `run_search_html` 等 **20 个真实调用者**。即 **CodeGraph 用 tree-sitter + 名称匹配为 Rust 合成了调用边**。
- **GroundGraph 的对应能力缺口（头号问题，已于 0.10/0.11 关闭）**：GroundGraph 的 tree-sitter 层原先**只产结构边（contains/imports）+ 测试调用识别，不产函数→函数调用边**；调用边**仅来自 LSP/analyzer 层**。于是 **Rust / C / C++ / Java / Go 等纯 tree-sitter 语言没有调用图**，`callers` / `dead-code` / `impact` 在这些语言上退化。→ **现已解决**：0.10 落地 Rust/TS/JS 启发式调用解析、0.11 泛化到 Go/Python/Java/C/C++/Swift，9 门 tree-sitter 语言全部产 medium 置信 `Calls`/`References`（真实仓验收见 0.11，如 SQLCipher 4253、ReactiveSwift 627）。
- **覆盖缺口**：GroundGraph 的 TS 适配器只认 `.ts/.mts/.cts/.tsx`，**不索引 `.js/.jsx/.mjs/.cjs`**（CodeGraph vendored 的 `dist` 全是 `.js`，因此「GroundGraph 扫 CodeGraph」在 vendored 副本上无文件可扫）——成熟标杆库应补 JS（可复用 TS 语法）。
- **CodeGraph 无 `dead-code` 命令**（只有 callers/callees/impact/affected）——**可达性死代码检测是 GroundGraph 独有能力**。

### 3) dead-code 是否正常：**设计正确，且精度随「精确边」线性变化**——已修一类假阳性

- **Dart（analyzer 精确层）= 精确**：tailorx 20 346 符号 / 5 212 入口 / 13 075 可达 / **298 高置信** / **0 警告**，0.58s。
- **Rust（无精确层）= 自知退化**：自身 2 264 符号 / **仅 2 入口 / 2 可达 / 2 262 判死**，但**如实打出**「无 calls/references 精确边，结果当候选而非结论」警告——honest-fail 生效。根因同上：Rust 没有调用边。
- **修复的真 bug（Dart 扩展成员假阳性）**：抽查 tailorx 的「高置信死代码」发现 `_buildLeftPanel`/`_hasVideo`/`_resultImageUrl` 等其实**在用**。根因：sidecar 的 `_DeclarationVisitor` **不进入 `ExtensionDeclaration`**，`extension _X on _State { … }` 里的私有成员从未进入 `Element→id` 映射 → 指向/源自它们的调用与引用边全被丢弃 → **每个私有扩展成员都被误判为高置信死代码**（仅本例就含 **51 个 `_build*` 方法** + 多个 getter）。
  - **修复**：sidecar 新增 `visitExtensionDeclaration`，按 `on` 类型把成员登记为 `dart_method::<file>#<OnType>.<member>`（与 tree-sitter 节点 id 一致），只补边、不重复发节点。先写失败测试→实现→ sidecar 7 测试全绿、Rust 侧 sidecar 验收测试全绿。
  - **真实验收**：用修复后的 sidecar 实跑 tailorx 该子树，**新产 71 条引用边**——`_buildLeftPanel`/`_buildRightPanel` 现被 `buildAiTryonResultContent`（State.build）**调用**、三个 getter 被兄弟扩展**引用**，假阳性类整体消除。

### 头号工程项（下一步「处理」的真正解）：tree-sitter 启发式调用解析层

上面 2)、3) 的 Rust 退化与 Dart 历史缺边，根因同一个：**调用边只依赖 LSP/analyzer**。CodeGraph 证明「tree-sitter + 名称/导入匹配」能以中置信度为**任意语言**合成调用图。建议新增一档**确定性启发式调用解析**（介于结构层与 LSP 层之间）：

- **定位**：medium 置信、**仅增不减**（永不覆盖 LSP/analyzer 的 high 边）、按语言可开关；进 `dead-code`/`callers` 可达性，但 `check`/`impact` 的判定仍以 high 边为准（守住「确定性/可审计」）。
- **路线**：R1 单文件同名解析（body 内标识符匹配同文件符号）→ R2 跟随 `imports` 跨文件 → R3 receiver 类型收敛降误报；先做 Rust 垂直切片（自身 dogfood 语言），再泛化。
- **不做**：不为此引入 embedding（见 §5.3），模糊召回永远是可关增量。

> 本轮代码改动：仅 Dart sidecar（`walker.dart` 扩展成员解析 + 1 个回归测试）与本文档；Rust workspace 未改动，sidecar 相关 Rust 验收测试通过。

## 0.10 第四轮（2026-05-31 · 启发式调用解析泛化到 TS/JS + JS 覆盖 + 两处自审修复）

承接 0.9 的「头号工程项」与两处覆盖/统计缺口，本轮把方案从 Rust 垂直切片**泛化到 TypeScript/JavaScript**（CodeGraph 同语言），并修掉审查中暴露的两个自身问题。全部 TDD（先失败测试→最小实现→真实仓验收），workspace 全绿、`fmt`/`clippy` 零告警。

1. **TS/JS 启发式调用解析（R1+R2 泛化）。** 给 `LangSpec` 的 `call_idents_of` 钩子接上 TS 实现：`foo()`→`Calls`、`this.method()`/`obj.method()`→按属性名 `Calls`、`new Widget()`→`Reference`；内建（`console.log`/`arr.map`）因只链同文件/导入符号而自然被滤掉。
   - **跨 `.ts`/`.tsx` 双 spec 合并解析**：`.ts`（`<T>` 泛型）与 `.tsx`（JSX）必须用不同语法，但共享同一符号命名空间。新增 `index_repo_with_spec_collect` 把两遍的符号/导入/待解析引用收集后**合一再解析**，于是 `app.js` 调用 `util.ts` 里的 `helper` 也能连边（旧的逐遍解析连不上）。
   - **真实验收（CodeGraph，TS/JS）**：启发式调用/引用边 **0 → 1843**；`dead-code` 的「可能死」**1327 → 549（−59%）**，且「无 calls/references 精确边，结果当候选」的警告**消除**——证明 0.9 §2 的 Rust 退化根因在 TS/JS 上同样被解掉。边稳定为 **medium 置信**（indexer 名 `typescript_treesitter`，既非 `_lsp` 也非 `dart_analyzer`）。

2. **JavaScript 覆盖（堵 0.9 §覆盖缺口）。** `.js/.jsx/.mjs/.cjs` 全部纳入索引（复用 JSX-aware TS 语法，`TSX_SPEC` 作超集），import 解析跨 `.js↔.ts` 双向命中；新增 `javascript`/`js` 配置别名（规整到 `typescript` 适配器，自动覆盖 JS）。
   - **真实验收**：CodeGraph vendored 副本里 **15 个 `.mjs` + 1 个 `.js`**（`scripts/*.mjs`、`npm-shim.js`）从「完全不可见」变为已索引。

3. **`dead-code` 统计修正（自审 bug）。** 表头 `可达` 旧实现把**测试用例节点**（生产可达根，但非代码符号）也计入，导致 `可达 1978 > 总符号 1402` 的反常。改为与 `总符号` 同口径（仅计代码符号，默认排除 TestCase/TestGroup 与测试文件助手）。
   - **真实验收**：CodeGraph `可达 630 ≤ 总符号 1402`；GroundGraph 自身 `总符号 1548 · 入口 741 · 可达 850 · 可能死 546`，表头自洽。

4. **SIGPIPE 不再 panic（CLI 管线礼仪）。** 作为「`grep` 的代码图替代品」，`groundgraph dead-code | head` 旧行为是 broken-pipe panic（退出码 101 + 回溯）。`main` 启动时复位 `SIGPIPE` 为默认处置（经 `sigpipe` crate 封装 `unsafe`，守住 workspace `unsafe_code = "forbid"`）。
   - **真实验收**：`… | head -2` 由 **101（panic）→ 141（干净 SIGPIPE 终止）**，stderr 无回溯，下游正常拿到前两行。

> 本轮代码改动：`crates/groundgraph-engine`（`typescript_treesitter.rs` 调用解析、`treesitter.rs` 双遍合并解析、`dead_code.rs` 可达统计、`config.rs`/`treesitter.rs` 的 `javascript` 别名）、`crates/groundgraph-cli`（SIGPIPE 复位、index 渲染加「References (heuristic)」）。新增/扩展测试：TS 调用/构造引用 scan 测试、TS 跨 `.ts/.js` 调用边集成测试、`javascript` 别名折叠测试、可达统计回归测试；全部绿。下一步：把启发式解析按同一 `call_idents_of` 套路补到 Go/Python/Java/Swift/C/C++（每语言配 scan 测试 + 双扫描验收）。

## 0.11 第五轮（2026-05-31 · 启发式调用解析泛化到全部 6 门剩余语言 + Python 死循环真 bug 修复）

承接 0.10 的「下一步」，本轮把 `call_idents_of` 启发式调用解析**一次性补齐 Go / Python / Java / C / C++ / Swift**，至此 **0.9「头号问题」彻底关闭**：Rust / C / C++ / Java / Go「纯 tree-sitter 语言没有调用图」的退化不复存在。全部 TDD（先失败 scan 测试→最小实现→真实仓验收），workspace 全绿、`fmt`/`clippy` 零告警（engine lib **379 测试**）。

1. **六语言启发式调用解析（R1+R2 套路统一落地）。** 每门语言按其 tree-sitter 文法识别调用/构造节点，抽出 callee 裸名后走同一 `resolve_heuristic_refs`（同文件名称解析 + 跟随 import 跨文件，medium 置信、仅增不减）：
   - **Go**：`call_expression`（含 `pkg.Fn` selector）+ `composite_literal`（`Repo{}` 构造）。
   - **Python**：`call` + `attribute`（`obj.method`）。
   - **Java**：`method_invocation` + `object_creation_expression`（`new Greeter(...)`）。
   - **C**：`call_expression`（含函数指针 `s.cb()` 的 `field_expression`）。
   - **C++**：`call_expression` + `new_expression`，`qualified_identifier`（`Class::method`）/`template_function` 一律取**尾部裸名**走同一解析路径。
   - **Swift**：`call_expression` + `navigation_expression`（`obj.method()`）。
   - 至此 `rust_treesitter.rs` 的回归测试由「只有 Rust 有解析器」改为 `every_language_spec_opts_into_the_call_resolver`——**断言全部 tree-sitter spec opt-in**（自 C#/Ruby/PHP/Kotlin 落地后已扩到 13 门：Rust/TypeScript/TSX/Python/Go/Java/C/C++/Swift/C#/Ruby/PHP/Kotlin；仅 Dart 走 analyzer 精度层，不在此列）。

2. **Python `src_roots` 死循环真 bug 修复（生产级缺陷）。** 激活 Python 解析器后，索引带**仓库根级 `__init__.py`** 的工程（如 deer-flow 的 `src/` 整树）会 90% CPU **无限挂起**。根因：`python_src_roots` 的父目录回溯循环——根级 `__init__.py` 让 `init_dirs` 含空串 `""`，而 `"".rfind('/')` 恒得 `""`、`init_dirs.contains("")` 恒真，回溯永不终止。修复：当 `parent == cur`（已到仓库根，无更高目录可攀）即 `break`。这是 `src_roots_of` 在**每次索引开头无条件调用**的早期阶段，与解析器本身无关，但被新解析器激活的完整流水线暴露。先写回归测试 `src_roots_terminate_when_repo_root_is_itself_a_package`（fix 前挂死、fix 后秒过）→ 实现。
   - **真实验收**：deer-flow `src/`（70 Python 文件，根级 `__init__.py`）由**无限挂起 → 0.247s 完成**，产 29 条 `calls` 启发式边；最小复现工作区（9 文件、根 `__init__.py`）0.045s 完成。

3. **真实仓双扫描验收（LSP/analyzer 全关，纯启发式边）。** 目标仓 rsync 到 `/tmp` 副本、0 写回源仓，`lsp_command` 指向不存在二进制强制走 AST：

   | 语言 | 真实仓 | 文件 | 启发式 `calls` 边 | 墙钟 | 备注 |
   |---|---|---|---|---|---|
   | Swift | ReactiveSwift `Sources/` | 21 | **627** | 1.04s | 748 符号 |
   | C | WCDBOptimizedSQLCipher（SQLite 衍生） | 97 | **4253** | 6.95s | +183 imports |
   | C++ | gba-mus-ripper（自包含工具） | 7 | **290** | 1.10s | +4 references |
   | Python | deer-flow `src/` | 70 | **29** | 0.247s | 死循环修复后 |
   | Go | clash（前轮） | — | **1143** | — | +178 references |
   | Java | 合成多文件 `app` 包 | 2 | 2（同文件 `greet→format`/`main→run`） | — | 跨文件同包无 import → 按设计不连（保守、避免假链） |

   - **跨文件同包不连边是设计而非缺陷**：`resolve_heuristic_refs` 对裸名仅解析「同文件符号」或「显式 import 目标文件的符号」；Java 同包 `greeter.greet()` 无 import 语句故不连。所有语言一致，守住 medium 置信「仅增不减、宁缺勿错」。

> 本轮代码改动：`crates/groundgraph-engine`（`go_treesitter.rs` / `python_treesitter.rs` / `java_treesitter.rs` / `c_treesitter.rs` / `cpp_treesitter.rs` / `swift_treesitter.rs` 各接 `call_idents_of` + scan 测试；`python_treesitter.rs` 修 `src_roots` 死循环 + 回归测试；`rust_treesitter.rs` 改 `every_language_spec_opts_into_the_call_resolver`）。`fmt`/`clippy` 零告警，workspace 全绿。

---

## 0.12 第六轮（2026-06-04 · 业务文档流水线 `propose → business-doc`：补齐 CodeGraph 根本不做的事）

前五轮都在「缩小与 CodeGraph 在**代码检索广度/性能**上的差距」。本轮转向**护城河本身**：GroundGraph 的北极星是「**从代码构建可信业务文档**」，而 CodeGraph 的定位是「给 AI agent 的高性能代码上下文」——它没有业务声明、没有人工确认、没有面向人的文档产物。本轮把这条价值链**端到端跑通并产品化**，全程 TDD。

**问题诊断（为什么之前到不了「业务文档」）：**

- P9 早已定义了业务候选的**落盘格式**（`.groundgraph/candidates/business_logic.yaml`）与**人工确认闭环**（`groundgraph candidate review`），但候选的**生成**全靠人手翻图；
- 唯一的「喂 AI」入口 `connect propose` 是**链路级**（孤儿符号/测试清单），且在真实大仓上**分钟级超时**（tailorx 上 >6 分钟未结束），无法用于生成业务文档；
- `export` 命令只是把 DB 表 dump 成 JSONL，**不是面向人的业务文档**。

**本轮交付（两个新命令，闭合整条流水线）：**

```text
groundgraph propose        代码/文档/测试事实  → 业务模块证据包 + 中文提示词
  → AI 写 business_logic.yaml（仅基于证据 id，不臆造）
  → groundgraph candidate review   人工确认 accepted / rejected / needs_changes
  → groundgraph business-doc       已确认候选 + 解析回的证据 → 可读业务文档
```

1. **`groundgraph propose`（`business_pack` 引擎）——证据包生成。** 用仓库自身的 feature 目录约定（`lib/features/<x>`、`src/<x>`…）做**路径感知**的业务模块切分（区别于 `features` 的调用耦合聚类，更贴近人对业务的直觉）；单次内存加载图，按模块汇总语义信号（框架角色 / 路由 / Provider / 存储 / 流订阅 / 代表性入口符号 / 相关文档/测试 / 跨模块依赖），并附**约束式中文提示词**（只能引用证据包里真实出现的 id）。输出 json（喂 AI）/ markdown（含 Mermaid 依赖图 + 候选骨架 + 提示词）/ text。
   - **质量收敛（均失败测试先行）**：文档/测试**按文件去重**（原按 section/case 重复几十次）、剔除 `test/docs/tool` 等**非业务脚手架桶**、入口符号**排除测试目录的 mock/fake 与 freezed/.g.dart 生成代码**、Mermaid 依赖图只留权重前 24 条边。

2. **`groundgraph business-doc`（`business_doc` 引擎）——业务文档导出。** 默认只导出 **accepted** 候选（可信视图），`--include-proposed` 预览草稿；把每条候选的 `evidence` id **回解析到代码图**，按角色分组（代码/文档/测试/信号），引用真实 `path:line`，可逐条审计、可跳转；并做**证据漂移检测**（引用符号若已不在图中→单列告警，是 doc-code drift 的信号）。

3. **tailorx 真实端到端验收（27,368 符号 / 1,108 文档 / 2,569 测试）：**

   | 阶段 | 命令 | 结果 |
   |---|---|---|
   | 证据包 | `propose` | 25 个业务模块，**约 2 秒**（`connect propose` 曾 6 分钟未结束） |
   | AI 候选 | 人工以证据包为据写 `business_logic.yaml` | 4 条候选（auth / order_measure / cart / design），证据 id 均取自证据包 |
   | 人工确认 | `candidate review --accept` ×3 | auth / order_measure / cart 置 accepted，design 留作草稿 |
   | 业务文档 | `business-doc` | 导出 3 条业务能力，证据**全部解析（0 漂移）**，每条带 `path:line`、开放问题、风险、审阅留痕 |

   - 证据包质量修复**在真实数据上可见**：cart 模块入口由「freezed `$...CopyWith` 噪声」清理为 `CartRepository / CartApiClient / FinalizeDraftOrdersUseCase` 等真实业务入口。

**与 CodeGraph 的对照（本轮新增差异化）：** CodeGraph 能告诉 agent「这段代码调用了什么」；GroundGraph 现在能产出「**这个模块在产品里负责什么业务、依据哪些可审计的代码/文档/测试证据、由谁在何时确认**」。前者是检索，后者是**带信任链的业务知识资产**——这是 CodeGraph 架构里没有的层。

> 本轮代码改动：新增 `crates/groundgraph-engine/src/business_pack.rs`、`business_doc.rs`；新增 CLI `commands/propose.rs`、`commands/business_doc.rs` 并接入 `main.rs`。`fmt`/`clippy` 零告警，workspace 全绿（engine lib **404 测试**）。

---

## 0.13 第七轮（2026-06-04 · 业务模块改为「以代码图为准」+ 对标 GitNexus 的社区检测，可视化回归业务视角）

第六轮的 `propose` 用**路径约定**（`lib/features/<x>`）切业务模块——这隐含了「目标仓自身是规整分模块的」假设。真实仓往往**管理混乱**（扁平 `lib/`、按层切的 `lib/models`+`lib/services`+`lib/widgets`、或纯粹一团乱）。本轮把模块划分**改为以代码图为准**，并据此校准可视化方向。

**核心算法变更（全程 TDD）：**

1. **确定性 Louvain 社区发现（新 `feature_cluster.rs`）。** 在「文件级」加权无向图上跑模块度最大化的社区发现：把符号级 `calls`/`references`（权 2）/`imports`（权 1）耦合上提到文件对。一个业务特性内部 bloc→usecase→repository→api-client 互相调用稠密、跨特性稀疏，所以社区天然对齐业务特性——**与目录怎么摆无关**。纯函数、无依赖、节点序遍历 + 模块度增益平手偏向低社区号 + 末位按最小成员重排，**跑两次字节一致**（6 个单测：空图/无边/两三角桥接/团/带权覆盖稀疏目录/确定性）。

2. **`business_pack` 改为图驱动划分（路径降级为「命名/兜底」）。** 放置规则按置信度分两档：
   - **显式 feature 目录优先**：落在 `.../features/<x>/...`、`.../modules/<x>/...` 下的文件直接归到模块 `<x>`——这是作者**亲手画的业务边界**，图也总是认同（高内聚）。它还**溶解了社区检测会产生的跨特性「基础设施大团」**（每个特性的 repository 都互相耦合，否则会黏成一坨）。
   - **其余一律跟代码图**：没有显式 feature 标记的文件（扁平 `lib/`、按层切、乱放）按 Louvain 社区落位。**松散路径约定（`lib/<x>`、首层目录）一律不信**——「仓库管理混乱」正是栽在这里，此处以图为准。
   - **同名归并**：派生出同一 slug 的多个社区合并为一个模块（如无 feature 标记的 `lib/core` 会裂成几个松耦合社区、都自命名 "core"，归并后避免 `core / core_2 / …` 噪声）；两个真特性永不相撞（名字不同）。
   - **命名**：feature 标记直接用标记名；否则取主导 feature 目录 token；再否则取**最被引用的业务符号**（类型优先、滤掉 `load/default/fromJson` 等无业务含义词）；最后退化到公共目录。

3. **新增「内聚度」业务信号。** 每个模块给出 `内部耦合 / (内部+外部)` 的内聚度（GitNexus 也有 cohesion）：高=自洽的业务边界，低=与其它模块纠缠（重构信号，提示给 AI/人）。`propose` 文本/markdown 直接以「内聚 90%（高内聚，边界清晰）」呈现。

**对标 GitNexus（本轮新参考，`docs/sourcecode/gitnexus`）：** GitNexus 的管线是 **Leiden 社区检测 → 启发式标签 → LLM 直接写 name/description 落库**（`cluster-enricher.ts`），可视化是 Sigma.js/WebGL 的通用符号图。它印证了「**社区检测 = 以代码图为准**」这一方向（Leiden 是 Louvain 的后继）。但其业务理解是**未经核验的 LLM 猜测**、无证据约束、无人工确认、无漂移检测；图也正是用户说的「酷炫但无意义」。GroundGraph 的差异化因此更清晰：

| 维度 | GitNexus | GroundGraph |
|---|---|---|
| 模块来源 | Leiden 社区（图驱动） | Louvain 社区（图驱动）**+ 显式 feature 边界优先**——乱仓/规整仓都稳 |
| 业务名称/描述 | LLM 直接写、落库即终态 | AI 仅基于**证据 id** 提议 → **人工确认** → 落库；带审阅留痕 |
| 可信度 | 无核验 | 证据回解析到 `path:line` + **doc-code 漂移检测** |
| 可视化 | 通用符号知识图（WebGL，炫但泛） | **业务模块依赖图**（节点=业务模块、边=跨模块依赖）+ 内聚度 |

**验收（两类仓，全部以真实命令输出为准）：**

| 仓 | 形态 | 结果 |
|---|---|---|
| tailorx（27,368 符号，规整 `lib/features/`） | 规整 | **24 个业务模块**全部为真实特性（design 内聚 90%/order_measure/customer_edit/auth/ai_tryon 74%/measure_core/cart 66%/fabric 71%/settings/order 73%/customer_detail/products 73%/photo_measure/…），`lib/core` 归并为**单个**低内聚 infra 模块（内聚 12%，诚实标注为跨切面胶水，也确实是依赖图的中心枢纽），无碎裂/无脚手架泄漏 |
| `/tmp/messyshop`（**按层切**：`lib/models`+`lib/services`+`lib/pages`，无 `features/`） | 混乱 | `init→index→propose` 端到端：还原出 **`user` / `order` 两个业务模块**（各跨三层、各 3 文件），业务依赖图 `order --> user`——路径切分只会给出 "models/services/pages" 三个**架构桶** |

**性能（release `propose`，tailorx 27,368 符号 / 144MB graph.db，三跑）：** 冷 1.58s → 0.72s → 暖 **0.21s**（`connect propose` 旧链路曾 6 分钟未结束）。可视化是 **24 节点 / ≤24 边的业务模块依赖图**（Mermaid `flowchart`），节点=业务模块、边=跨模块依赖，`core` 作为枢纽一眼可辨——对照 GitNexus 是把上万符号铺成 WebGL 力导图。

**质量修复（均失败测试先行）：** 入口符号排除 Dart `<default>` 合成构造器等无名/合成符号；脚手架 denylist 不再被 `_2` 后缀绕过（同名归并后 `test` 桶直接命中 denylist）。

> 本轮代码改动：新增 `crates/groundgraph-engine/src/feature_cluster.rs`（Louvain）；重写 `business_pack.rs` 的模块划分为图驱动 + feature 标记优先 + 同名归并 + 内聚度；`commands/propose.rs` 呈现内聚度。`fmt`/`clippy` 零告警，workspace 全绿（engine lib **411 测试**，CLI bin **42 测试**）。

---

## 1. 两个项目的画像

### 1.1 GroundGraph 现状画像

| 项 | 现状 |
|---|---|
| 实现语言 | Rust，6 crate：`core` / `store` / `lang-dart` / `engine` / `cli` / `mcp`，约 49k 行 |
| 存储 | SQLite（`nodes` / `edge_assertions` / `evidence` / `symbol_ranges` / `file_index` / `slice_cache`） |
| 解析后端 | 三层：Tier 2 进程内 tree-sitter 广度后端（通用 `LangSpec` 驱动，Rust/TS/Python/Go/C/C++）+ Tier 3 LSP sidecar 精度层（Dart analyzer / pyright·basedpyright·pylsp / gopls / sourcekit-lsp / typescript-language-server / jdtls）+ Rust/AST fallback（无 LSP 时 soft-skip） |
| 支持语言 | LSP 精度层：Dart、Swift、Go、Python、TypeScript、Java；tree-sitter 广度层：Rust、TypeScript、**JavaScript（`.js/.jsx/.mjs/.cjs`，复用 JSX-aware TS 语法）**、Python、Go、C、C++（去重共 **9 门**：Dart/Swift/Go/Python/TypeScript/Java/Rust/C/C++） |
| 启发式调用解析 | tree-sitter 进程内、medium 置信、仅增不减的 `Calls`/`References` 合成层（同文件名称解析 + 跟随 import 跨文件；TS 跨 `.ts/.tsx` 双 spec 合并解析）：**9 门 tree-sitter 语言全部落地**——Rust、TypeScript/JavaScript（0.10）+ Go、Python、Java、C、C++、Swift（0.11）；`every_language_spec_opts_into_the_call_resolver` 守门。仅 Dart 走 LSP/analyzer 精度层不在此列 |
| 索引刷新 | 手动 `groundgraph index`（带 file-hash 增量），**无文件监听 / 无 auto-sync** |
| 代码事实边 | `contains` / `imports` / `calls` / `references` |
| 语义边 | `reads_provider` / `navigates_to` / `persists_to` / `subscribes_stream`（Flutter/Riverpod 专项）；Python 框架装饰器（FastAPI/Flask/Celery/Click/Pydantic）识别 |
| CLI 命令面 | `init/index/slice/impact/check/context/connect/export/graph/candidate/logic/`**`propose`**`/`**`business-doc`**`/search/dead-code/similar/select-tests/features/graph-diff/questions`（`propose`+`business-doc` 为业务文档流水线两端） |
| MCP 工具 | 7 个：`search_graph` / `context_pack` / `explain_symbol` / `get_subgraph` / `impact` / `dead_code` / `check_drift`（独立 `groundgraph-mcp` 二进制） |
| 可视化 | 自包含、离线、零 CDN 的 HTML 代码图浏览器（Documents / Business / Code / Tests / Risks 五泳道，fact/confirmed/candidate/risk 四图层） |
| 成熟度 | v0.2.0 已收口发版，v0.3.0-A（置信度贯通）+ P21/P22（tree-sitter 广度后端）+ P23（9 语言启发式调用解析全覆盖）+ P24（业务文档流水线 `propose`/`business-doc`，**模块划分以代码图为准**：Louvain 社区 + 显式 feature 边界优先）已落地未发版；workspace 全绿（engine lib **411 测试** + CLI bin **42 测试**，含逐语言属性测试 + 9 语言调用解析 scan 测试 + 业务证据包/文档导出测试 + Louvain 社区发现测试） |

**核心价值链（产品主线）：**

```text
文档事实 / 代码事实 / 测试事实
  → groundgraph propose：按业务模块聚合的证据包 + 约束式中文提示词
  → AI 生成中文业务逻辑候选 + 候选关联（带 evidence / 可信度 / open questions）
  → groundgraph candidate review：人工确认（accepted / rejected / needs_changes / pending）
  → confirmed graph
  → groundgraph business-doc：可读业务文档（证据回解析、path:line、漂移检测）
  → PR Impact / Agent Context Pack / 图浏览
```

**不变的架构信条（来自 PRD §15）：**

```text
1. Graph is not truth. Evidence is truth.
2. LLM suggests. Human confirms.
3. CI trusts only deterministic or confirmed edges.
4. PR Impact is the main engineering value.
5. Rules don't infer business links; AI proposes, humans confirm.
```

### 1.2 CodeGraph 画像（标杆）

| 项 | 现状 |
|---|---|
| 实现语言 | TypeScript / Node，`src/` 118 文件；npm 包 `@colbymchenry/codegraph` |
| 分发 | 自包含 bundled binary（内置 Node runtime，免编译），一行 curl/irm 安装；交互式安装器自动配置 8 个 agent |
| 存储 | SQLite + FTS5 全文搜索 |
| 解析后端 | tree-sitter（bundled wasm），确定性 AST 抽取，**非 LLM 总结** |
| 支持语言 | 20+（TS/JS/Py/Go/Rust/Java/C#/PHP/Ruby/C/C++/ObjC/Swift/Kotlin/Scala/Dart/Svelte/Vue/Liquid/Pascal/Lua/Luau） |
| 索引刷新 | 原生 FSEvents/inotify/RDCW 文件监听 + 防抖 auto-sync + 连接时 catch-up + per-file staleness banner |
| 代码边 | `contains/calls/imports/exports/extends/implements/references/type_of/returns/instantiates/overrides/decorates` |
| 动态分发 | 合成器闭合静态解析断裂的流：callback/observer、EventEmitter、React re-render（setState→render）、JSX child；跨语言桥 Swift↔ObjC、RN bridge/TurboModules/Fabric、Expo Modules |
| 框架路由 | 14 框架（Django/Flask/FastAPI/Express/NestJS/Laravel/Drupal/Rails/Spring/Gin/Axum/ASP.NET/Vapor/React Router/SvelteKit）→ `route` 节点 |
| MCP 工具 | 10 个：`search/context/trace/callers/callees/impact/node/explore/files/status`；MCP `initialize` 自带 agent 使用指南（单一真相源） |
| 基准方法学 | 严格 A/B：with vs without，7 真实仓库 ×7 语言，median-of-4，公开「~25% 更便宜 / 57% 更少 token / 62% 更少 tool call」 |
| 成熟度 | v0.9.7，npm 公开发布，完整 release 工程（per-platform bundle + installer 契约测试） |

**核心价值：** 给 agent 一份预建知识图，让它**直接回答结构问题而不 grep/read**。CLAUDE.md 把目标说得很死：优化的是**墙钟延迟 + tool-call 数**，不是 token cost；判据只有一个 —— *codegraph 的答案是否「足够」到让 agent 停止去 Read*。

**关键工程哲学（强烈值得记住）：**

> **"Adapt the tool to the agent — don't try to change the agent."**
> 影响 agent 的渠道（MCP instructions、tool description）都是低权重的，改文案无法可靠改变 agent 的工具选择。能落地的只有：让 agent **已经会调**的工具，对它**已经会给**的输入，产出**更充分**的结果（sufficiency），以及扩大静态可连接的覆盖（coverage）。

---

## 2. 能力对比矩阵

| 维度 | GroundGraph | CodeGraph | 谁更强 |
|---|---|---|---|
| 实现 / 分发 | Rust 静态二进制；目前仅 macOS tar 包，依赖外部 LSP | Node bundled binary，一行装，8 agent 自动配置 | CodeGraph（分发工程学） |
| 解析后端 | 三层：tree-sitter 广度（Tier 2，进程内零依赖）+ LSP 精度（Tier 3）+ AST fallback | tree-sitter（快、零依赖、bundled） | 趋同（GroundGraph 也有进程内 tree-sitter 了，见 §5.1） |
| 语言广度 | 9（tree-sitter 广度层 6 + LSP 精度层 6，去重 9） | 20+ | CodeGraph（仍领先，但差距收窄） |
| 索引新鲜度 | 手动 index | 自动 auto-sync + staleness banner | CodeGraph |
| 调用链追踪 | `slice --call-depth` fanout | `trace`（一次返回完整路径 + body inline + 动态 hop） | CodeGraph |
| 动态分发 / 跨语言桥 | Flutter/Riverpod 专项语义边 | 通用合成器 + iOS/RN/Expo 跨语言桥 | CodeGraph |
| 框架路由 | Python 装饰器 | 14 框架 route 节点 | CodeGraph |
| 影响分析 | `impact`：改动 → **需求/文档/测试**，带真实边轨迹 | `impact`：符号影响半径；`affected`：改动→受影响测试 | GroundGraph（影响到意图层面） |
| **需求↔代码↔测试可追溯** | ✅ 核心能力 | ❌ 明确不做 | **GroundGraph（护城河）** |
| **AI 候选 + 人工确认闭环** | ✅ candidate→confirm，候选永不自动入信任图 | ❌（确定性，无 human-in-loop） | **GroundGraph（护城河）** |
| **证据模型** | ✅ 每边带 certainty/source/confidence/evidence_quality，可审计 | `provenance:'heuristic'` 标记 | **GroundGraph** |
| **Doc-Code 漂移检测** | ✅ Logic Confidence（stale/missing_doc/missing_link/mismatch） | ❌ | **GroundGraph** |
| 业务可视化 | ✅ 离线 HTML，区分 fact/confirmed/candidate/risk | ❌（有文档站，无图浏览器） | GroundGraph |
| 死代码 / 重复 / 测试选择 | `dead-code`/`similar`/`select-tests`/`features`/`graph-diff`/`questions` | `affected`（测试选择） | GroundGraph（治理命令面更广） |
| 非侵入硬约束 | ✅ 只写 `.groundgraph/`，shadow-scan 证 0 副作用 | 写 `.codegraph/`（无业务文档纪律） | GroundGraph |
| MCP / agent 工程学 | 6 工具，instructions 是一句话 | 10 工具，instructions 是单一真相源 + "直接信任别 re-verify" | CodeGraph |
| 基准严谨度 | 规模 scan 报告（节点/边/符号计数） | 对照 A/B + 公开收益数字 | CodeGraph |
| 成熟度 | v0.2 / v0.3.0-A | v0.9.7（npm 公开） | CodeGraph |

**重叠区**（拼不过、别硬拼）：纯代码检索、多语言结构图、calls/callees、impact-on-code、MCP 检索工具。
**分化区**（GroundGraph 独占、应加固）：意图/需求对齐、证据 + 人工确认、Doc-Code 漂移、PR 影响到需求/文档、可审计 provenance、离线业务图浏览。

---

## 3. CodeGraph 值得借鉴之处（可落地清单）

按「投入产出比 + 与护城河的兼容性」排序：

### 3.1 检索充分性（sufficiency）—— 立刻借
- **`trace` 式工具**：给两个符号，一次返回完整调用路径 + 每跳 body inline。GroundGraph 现在要 `slice` 多跳 + 人脑拼，agent 体验差。建议给 MCP 加 `trace_path(from, to)`。
- **`context_pack` 自适应大小**：CodeGraph 的 `explore` 按「答案」而非「文件数」裁剪输出（大文件里只给问到的方法 + 机制，把可互换实现折叠成签名）。GroundGraph 的 `context_pack` 可引入同样的预算策略。
- **MCP instructions 作为单一真相源**：把「直接信任结果、别再 grep 复核、按 intent 选工具」写进 `initialize` 响应，而不是只给一句话。这是几乎零成本、直接提升 agent 配合度的改动。

### 3.2 新鲜度（freshness）—— 高价值
- **文件监听 + auto-sync + staleness banner**：CodeGraph 的「编辑→防抖→重索引→下次查询可见」+「未同步文件加 ⚠️ 提示让 agent 直接 Read」。GroundGraph 当前手动 `index`，在 agent 长会话里会给过期答案。Rust 侧可用 `notify` crate 实现，难度可控。
- **连接时 catch-up**：MCP server 启动先做 `(size, mtime)` + content-hash 对账再答第一个查询。

### 3.3 分发工程学（distribution）—— 决定能否被用起来
- **一行安装 + agent 自动配置**：CodeGraph 的 installer 自动探测并写好 8 个 agent 的 MCP 配置。GroundGraph 是 Rust，做**全平台静态单二进制**比 Node 更容易，反而落后了。建议补：跨平台 release（Linux/Windows/macOS ×x64/arm64）+ `groundgraph install` 自动配置 Cursor/Claude Code/Codex。
- **installer 契约测试**：CodeGraph 有 ~47 个参数化契约测试锁死「安装幂等、卸载可逆、re-run 字节一致」。

### 3.4 覆盖率（coverage）—— 中期
- **动态分发合成器**：callback / EventEmitter / 框架回调这类静态断裂，CodeGraph 用合成边闭合，并标 `provenance:'heuristic'` + `synthesizedBy`。GroundGraph 已有 Flutter 专项语义边，可推广为「通用合成器 + provenance 标记」，正好喂给 AI 候选层做证据。
- **tree-sitter 作为「广度层」后端**：见 §5.1。

### 3.5 度量文化（measurement）—— 持续
- **A/B 基准方法学**：small/medium/large 真实仓 × ≥3 个流程问题，with vs without，≥2 run 取中位。GroundGraph 应建立自己的对照实验，但**度量目标要换**：不是「省 token」，而是「**REQ-aware 上下文是否减少 AI 改错 / 返工 / 漏测**」——这才是意图层的价值证明。

> ⚠️ 一条**反向教训**（CodeGraph 已踩过）：*"partial coverage is WORSE than none"* —— 桥一半的流会暴露一个 hop 让 agent 去 drill+read，反而更糟。GroundGraph 加合成边时必须端到端闭合再上线。

---

## 4. GroundGraph 的差异化优势（护城河）

这些是 CodeGraph **结构上不做**或**做不到**的，必须持续加固：

> **头号差异化（0.12 起已端到端落地）：从代码生成可信业务文档。** `propose → AI 候选 → candidate review → business-doc` 这条流水线，把「代码事实」变成「带信任链、可审计、面向人的业务知识资产」。CodeGraph 是「给 agent 的代码检索」，到此为止；GroundGraph 多出「业务声明 + 人工确认 + 文档产物」整层。详见 0.12。

1. **意图 ↔ 代码 ↔ 测试的可追溯闭环。** CodeGraph 只有代码结构；GroundGraph 把「为什么有这段代码（需求）/谁验证它（测试）/它在哪记录（文档）」连起来。这是 AI 编程当前最大的盲区（AI 不知道需求、不知道代码为何存在、不知道该读哪些测试）。
2. **证据优先 + 人工确认的信任模型。** "Graph≠truth, Evidence=truth；LLM 建议，人确认；CI 只信确定边或确认边。" 这让结果可进 CI、可审计、可问责，而纯启发式图做不到。
3. **每条边可审计的 provenance。** `certainty / source / confidence / evidence_quality` 四维，远比 `provenance:'heuristic'` 单标记丰富，是「向人 / 向 CI 解释为什么信这条边」的基础设施。
4. **Doc-Code 漂移检测（Logic Confidence）。** `stale_link`（hash 变了没复核）/ `missing_doc` / `mismatch_candidate`，直接对应「文档说 A、代码做 B」这类 AI 最容易被误导的场景。CodeGraph 完全没有这一层。
5. **PR 影响打到需求 / 文档层面**，不止代码影响半径——能回答「这个 PR 动了哪些业务需求、相关文档要不要改、对应测试变没变」。
6. **非侵入硬约束 + shadow-scan 证 0 副作用。** 业务代码/文档/测试只读，只写 `.groundgraph/`。对「不想被工具污染仓库」的团队是强卖点。
7. **离线、自包含、区分图层的业务图浏览器**（fact/confirmed/candidate/risk），让人能一眼看清「哪些是事实、哪些是 AI 猜的、哪些已确认、哪些有风险」。
8. **Rust 内核**：`unsafe` forbidden、单静态二进制、无 GC/runtime，长期在性能与分发上有上限优势（只是目前没兑现成分发能力）。
9. **更广的工程治理命令面**：dead-code / similar / select-tests / graph-diff / questions —— 这些围绕「图」长出的治理能力，CodeGraph 基本没有。

---

## 5. 关键架构判断

### 5.1 解析后端：不要二选一，做三层

CodeGraph 全押 tree-sitter（广、快、零依赖，但不 resolve 类型/跨文件引用精度有限）；GroundGraph 押 LSP（精度高，但要外部 LSP 在 PATH、慢、依赖环境）。**建议做分层后端，按场景取舍**：

```text
Tier 1  lightweight (Rust 原生扫描)  — 永远可用的兜底
Tier 2  tree-sitter (linked-in)      — 结构层（P23 起为唯一结构来源）：Rust/TS/Python/Go/Java/Swift/Dart/C/C++
Tier 3  可选富化 (LSP / Dart analyzer; 规划中的 SCIP 摄入) — 精度层：按符号 id 叠加 calls/references/语义边
```

> **P23 收敛（已落地，未发版）**：原先每门 LSP 语言并存「手写 `*_ast.rs`（LSP）+ `*_treesitter.rs`（通用驱动）」两套结构通路且 id 不一。P23 把 tree-sitter 通用驱动确立为**唯一结构来源**，Python/TS/Java/Go/Swift/Dart 全部收敛（删除各自 `*_ast.rs`，新增 `tree-sitter-java`/`tree-sitter-dart`），LSP / Dart analyzer 降为**可选 Tier-3 富化**（按符号 id 零翻译叠加语义边）。配置统一为 `languages:` + `enrichment:`（旧键弃用别名）。精度层下一步按 [ADR-0001](adr/0001-scip-and-stack-graphs.md) 向「离线 SCIP 摄入」迁移。

价值：用 Tier 2 廉价拿到 CodeGraph 级别的语言广度（结构/import/符号），把昂贵的 Tier 3 留给需要高可信调用链的语言；三层都打统一 `indexer` 标记进 evidence，可信度自然分级。这条路同时解决了「语言太少」和「依赖外部 LSP」两个短板。

**「Tier 2 框架」到底是什么（P22 已落地）：** 它不是「为每门语言各写一个扫描器」，而是**一个数据驱动的通用 tree-sitter 驱动**（`crates/groundgraph-engine/src/treesitter.rs`）：
- 遍历器 `extract` / `walk` 与索引器 `index_repo_with_spec` 只写一次、测一次、所有语言共享；嵌套限定名（`Outer::Inner::method`）从真实 AST 祖先链推导，跨语言统一，无需每语言维护「容器名单」。
- 每门语言坍缩成一份静态 `LangSpec`：`grammar`（编译期链入的语法）+ 扩展名 + 一把小函数指针钩子（`container_of` / `is_callable_kind` / `import_of` / `name_of` / `body_of` / `is_transparent_kind`），仅两处真正的不规则（Rust `impl` 块、Go 方法接收者）被隔离到 `impl_type_of` / `receiver_type_of`。加一门语言≈「换语法 + 填映射」，不再复制递归逻辑。
- 驱动**全函数、panic-free、深度受限**（`MAX_NESTING_DEPTH=256`）；编译期语法、运行期零外部进程、确定性。复用同一 `LanguageIndexBatch` 入库通路 + 逐语言 `proptest` 鲁棒性契约（任意输入不崩 + 确定性 + 符号良构）。
- 实测：P21 单语言（Rust 自举 125 文件 / 2023 符号）→ P22 通用化后六语言，单个 `treesitter:` 配置开关即可多语言索引；端到端集成测试 `p22_treesitter_multilang` 钉死六语言全部入库。

**是否还有更好的方案？** tree-sitter 仍是当前广度层的最优解，理由与备选取舍：
- **vs 各语言官方 LSP（我们的 Tier 3）**：LSP 精度更高（resolved 类型 / 跨文件 calls），但要外部二进制在 PATH、慢、环境脆（实测 Dart 22.9s、Swift ~80s 且大仓超时）——只适合做精度层，不适合做广度兜底。**结论：保留为 Tier 3，按语言选用。**
- **vs 自写正则 / 行扫描（我们的 Tier 1 / Python·Java AST fallback）**：零依赖但脆、易被字符串/注释误导、难维护——只配做「无语法时的兜底」。**结论：保留为 Tier 1。**
- **vs SCIP / stack-graphs（GitHub 的精确符号解析）**：能做到跨文件精确「定义→引用」，是比 tree-sitter 更强的*符号解析*层；但工程量大、每语言要写 stack-graph 规则、生态尚不如 tree-sitter 广。**结论（已升级为 [ADR-0001](adr/0001-scip-and-stack-graphs.md)）**：SCIP 是数据格式，采「**摄入优先**（离线 `.scip` 替代实时 LSP 作首选精度层，正面解诉求 #2）+ 导出其次」，绑定复用 P23.0 的 `symbol_ranges` **按范围零翻译叠加**（不引入第二套 id）；stack-graphs 作「无外部 LSP 的进程内跨文件解析」终极方向，单语言 spike、默认关闭。**实现默认延后**。
- **vs ctags / SemanticDB / 厂商 SDK**（TS Compiler API、JDT、roslyn 等）：要么精度不足（ctags），要么把「零运行期外部依赖 + 单静态二进制」的分发优势打掉（重型 SDK）。**结论：不引入。**
- 一句话：**广度层 = tree-sitter（P22/P23 已成唯一结构来源）；精度层 = 可选 Tier-3 富化（按符号 id 叠加 calls/refs/语义边），首选向「离线 SCIP 摄入」迁移、实时 LSP 退为回退；stack-graphs 列为终极方向（见 ADR-0001）。**

### 5.2 定位防线：别把纯代码检索当主战场

把产品叙事钉在「**意图治理 / 对齐**」，代码图是手段不是终点。对外一句话定位建议：

> 「面向 AI 编程的**意图对齐层**：用证据把需求、文档、代码、测试连起来；AI 提候选，人确认，CI 只信确认。」

CodeGraph 是「更快地找到代码」；GroundGraph 是「确保 AI 改对了**该改的东西**，且没让文档/测试/需求脱节」。二者甚至可以**共存**（底层借 CodeGraph 思路做检索，上层做对齐）。

### 5.3 是否引入 embedding / 向量检索：**默认不引入**（核心检索与信任路径）

**结论先行**：检索/信任主路径不引入 embedding。理由分三层——

1. **标杆事实：成熟项目并不靠它。** 实跑 CodeGraph v0.9.7 dist（137 个 Rust 文件 768ms 建库、query 70ms），其 `schema.sql` 用 **FTS5 全文索引 + B 树索引 + tree-sitter 确定性 AST**，**全程零 embedding / 零向量表**。它的「快且准」来自索引工程，不是语义向量。
2. **本轮实证：我们的慢是缺索引，不是缺语义。** `search "craft tree"` 230s 的真因是 `edge_assertions` 无邻接索引导致的全表扫描 + 加权未截断（见 §0.8），加索引后 0.06s，与 CodeGraph 同档。把语义向量贴上去既治不了那个病、又掩盖真因。
3. **与护城河信条冲突。** GroundGraph 的卖点是「**确定性 / 证据可解释 / 可进 CI**」：同一查询稳定可复现、每条命中可追到 evidence、CI 只信人确认的边。向量近邻是**概率近似、不可逐条解释、随模型版本漂移**，直接放进信任路径会破坏「可审计」这一核心承诺。

**何时才考虑、且只在隔离档位：** 仅当出现「跨自然语言/同义改写的模糊检索」这类**确定性手段根本覆盖不到**的需求时，才以**可选、默认关闭、旁路（只排序不进信任边）**的形式引入——即「AI 提候选→人确认→CI 只信确认」管道里的*候选生成器*，绝不参与 `check` / `impact` 的判定。这与 §5.1「stack-graphs 默认关闭的终极档」同构：**先把确定性做到标杆，模糊能力永远是可关的增量。**

---

## 6. 未来演进规划（分阶段）

> 原则：先借 CodeGraph 的「检索工程学 + 分发」补齐生存线，再all-in 护城河；每阶段配 A/B 证据，度量「减少 AI 返工」而非「省 token」。

### Phase A：生存线 —— 让人能用起来、用得新鲜（1–2 个迭代）
- [ ] 跨平台 release：Linux/Windows/macOS × x64/arm64 静态二进制 + 一行安装脚本。
- [ ] `groundgraph install`：自动探测并配置 Cursor / Claude Code / Codex / opencode 的 MCP（参考 CodeGraph installer 契约测试）。
- [ ] MCP `serve` 加**文件监听 auto-sync + staleness banner + 连接时 catch-up**。
- [ ] MCP `initialize` instructions 升级为单一真相源（按 intent 选工具、直接信任结果、漂移时提示 Read）。

### Phase B：检索充分性 —— 把 agent 体验拉到 CodeGraph 水平（2–3 个迭代）
- [ ] MCP 加 `trace_path(from, to)`：一次返回完整调用路径 + body inline。
- [ ] `context_pack` / `get_subgraph` 引入**自适应输出预算**（按答案而非文件数裁剪，大文件聚焦命中符号）。
- [x] tree-sitter 广度层（Tier 2 后端）**首语言 Rust 落地（P21，自举通过）**；**通用 `LangSpec` 驱动 + 推广到 Rust/TS/Python/Go/C/C++ 六语言（P22）**；**P23 收敛：tree-sitter 成为唯一结构来源，Python/TS/Java/Go/Swift/Dart 全部收敛（+`tree-sitter-java`/`tree-sitter-dart`），LSP/analyzer 降 Tier-3**；[ ] 继续补 C#/Ruby/Kotlin 等把结构层语言数推到 15+。
- [ ] 建立 GroundGraph 自己的 A/B 基准（含「REQ-aware context 降低改错率」的度量）。

### Phase C：护城河深化 —— 把对齐做成不可替代（持续）
- [x] **Markdown 需求映射（P23.9/P23.10）**：`.groundgraph/requirements/*.md`（中文 H1 编号+标题、`## 文档/实现/测试` 引用）→ `Requirement` + `Documents/DeclaresImplementation/DeclaresVerification` 边，`init` 脚手架、围栏/README 安全跳过、`links.yaml` 兼容；并用本格式为本仓写 18 条中文需求自举（`slice`/`impact`/`graph`/`check` 全连通、0 unresolved）。
- [ ] **业务候选生成流水线产品化**：从「人手喂 `business_logic.yaml`」升级为 `groundgraph propose` 自动产出候选证据包 → AI 生成中文候选 → 交互式确认（`groundgraph review --interactive`）。
- [ ] **Doc-Code 漂移变一等公民**：`check` 直接报 `mismatch_candidate` / `stale_link`，并能在 PR 里给「这次改动让哪些需求/文档可能过期」。
- [ ] **需求覆盖率指标**：多少需求有实现、有测试、有文档、链路新鲜——做成 CI gate 和看板。
- [ ] 通用动态分发合成器（callback/event/框架回调）+ provenance 标记，端到端闭合后再上线（吸取 CodeGraph 「half-bridge 更糟」教训）。
- [ ] Review Workflow（PRD Phase 2）：候选低成本批量确认。

### Phase D：生态 —— 成为 AI 编程工具链的一环（远期）
- [ ] 把 `slice` / `impact` / 漂移检测接入 CI（GitHub Action），PR 自动评论「影响的需求/文档/测试 + 漂移风险」。
- [ ] GraphRAG / 语义查询（PRD Phase 5），但严守「只用于查询/候选，输出必带 evidence，不作事实源」。

---

## 7. 命名建议（含 2026-05-31 占用核验）

用户诉求：**更短、更有象征性**。先说核验结论——**几乎所有「短 + 有象征意义」的英文单词在 crates.io 都已被占**（单一全局命名空间 + 抢注严重），尤其是织布/图/证据这些与产品同义象的词，且不少正落在我们赛道里：

| 候选 | 象征意义 | 占用核验（crates.io / npm / GitHub） | 结论 |
|---|---|---|---|
| **Weft**（纬线） | 经纬交织 | crates `weft`（HTML 模板）+ 热门 `WeaveMindAI/weft`（Rust+AI 语言，1.3k★） | ❌ 冲突且撞热点 |
| **Cairn**（路标石堆） | 非侵入的路径标记（极贴！） | `cairn-knowledge-graph`（静态规范分析 KG）、`cairn`（构建版本控制）、`cairn-p2p` | ❌ 同赛道已占 |
| **Splice**（拼接） | 把两股拧成一股 | `oldnordic/splice` = **7 语言代码图重构内核**（直接竞品！） | ❌ 正面撞车 |
| **Plait**（三股辫） | 文档+代码+测试三股交织 | crates `plait`（HTML 模板）+ npm `@plait/*`（白板框架，含 graph-viz） | ❌ 冲突 |
| **Skein**（一绞线） | 把缠结的线理顺 | crates `skein`（RustCrypto 哈希，24w 下载） | ❌ 冲突 |
| **Lode**（矿脉） | 贯穿代码的"真相矿脉" + lodestar 引导 | `lodepng`；GitHub 多个 "Lode"（AI coding agent / Ruby 包管理器） | ⚠️ 风险高 |
| **Attest**（证实） | "AI 提议、人确认"=attestation（贴护城河） | 语义被**供应链签名/Sigstore**强占（`@actions/attest`、npm provenance） | ⚠️ 语义冲突 |
| **Throughline**（贯穿线） | 需求→代码→测试→文档的那根主线 | 复合词，赛道内未见占用 | ✅ 可用性最好 |
| **SpecWeave** | 把 spec 织进 code/test | 复合词，极可能空闲 | ✅ 延续性最好 |

**给用户的决策（按诉求"短+象征"排序）：**

1. **若坚持 4–6 字母的极短符号**：真实单词基本都被占，必须**自造词**。推荐方向（最终发布前仍需一次 crates/npm 终检）：
   - **`Veris`**（拉丁 *verus*=真，"of truth"）——直指"证据=真相"内核，5 字母、好读、近乎空白命名空间。**短+象征的首选。**
   - **`Tess`**（tessellation 镶嵌，碎片拼成整体）——4 字母，呼应"把需求/代码/测试拼成完整图"。
2. **若可接受一个词稍长但一眼说清**：**`Throughline`**（核验最干净）——它本身就是产品隐喻（贯穿线），品牌/GitHub/CLI 直接用 `throughline`，crates 用 `throughline`/`throughline-core`。
3. **若优先迁移平滑**：**`SpecWeave`**（从 GroundGraph 自然演进）。

> 现实提醒：对 CLI 工具，真正决定身份的是 **GitHub 仓名 + CLI 命令名**，crates.io 句柄可加后缀（`<name>-core`）。所以不必因 crates 抢注而放弃好隐喻——但要避开 §上表里**同赛道的实义词**（cairn/splice/weft），否则会与现存竞品混淆。
>
> 我的最终建议：**短而象征 → `Veris`；稳妥描述 → `Throughline`。** 二者都比"切片"味的旧名更能把产品钉在"证据/对齐"内核上，与 CodeGraph 清晰区隔。

---

## 8. 风险与取舍速记

- **别在纯代码检索上和 CodeGraph 拼参数**（语言数、token 省多少）——那是它的主场，且它已 v0.9。借它的工程学，赢在对齐层。
- **加合成边/语义边必须端到端闭合**，半截桥比没有更糟（CodeGraph 实测教训）。
- **AI 候选永不自动入信任图**——这是 GroundGraph 可进 CI 的根本，任何「省事」的破例都会摧毁信任模型。
- **分发是当前最大短板**：Rust 本该比 Node 更易做静态分发，却落后于 CodeGraph 的一行安装，应优先补。
- **度量要换标尺**：用「降低 AI 改错/返工/漏测」证明价值，而不是套用 CodeGraph 的省钱叙事。

---

*生成时间：2026-05-31 · 对标版本：CodeGraph v0.9.7 / GroundGraph v0.3.0-A（+P21/P22 分支）*
*P25 更新（2026-06-10）：经典大库实战验证 — Redis（~20 万行 C，215 文件）与 fmt（header-heavy C++）。修复 7 项通用缺陷：`deps/` 第三方目录误收、C 跨翻译单元调用解析（可达 127→6379，死码误报 3472→896，high 置信仅剩 2 个且为真实备用链接实现）、C/C++ `main` 隐式入口、函数式宏建模为可调用符号、`typedef struct _Tag {} Name` 取 typedef 名、平铺布局模块命名（dominant-file stem：Networking/Db/Cluster Legacy）、header-only `include/` 并根（fmt 0→849 节点）。性能：index 11s / 200k 行，全部分析命令 <1s，无阻塞点。新增 `groundgraph dashboard` 离线 HTML 管理面板与 `p25_effectiveness_c_redis_patterns` 有效性黄金测试（真阳性+真阴性双向断言）。*

*P26 更新（2026-06-10）：issue 定位实验 + 四语言大库横评（gin/Go、django/Python、gson/Java、TypeScript 编译器仓）。**Issue 定位**：取 Redis 三个真实 open issue（#15078 zset 边界、#15080 TLS WANT flags、#15320 MSET dictFind）以标题词直接 search——修复前 `tools/array-bench.py::parse_qps`、`blpop_and_set_multiple_keys`（测试 helper）盖过真实目标；新增 `path_class::is_auxiliary_path` + 搜索末段路径类别降权（×3/4，作用于结构+内容+证据+邻接总分），三例目标全部进 top4（两例 top1）。**gin**：`testdata/` 列入 ALWAYS_SKIP_DIRS（Go 工具链约定，862→835 符号）；模块命名不再被测试文件 stem 污染（"binding_test/teststruct/cleanpathtest"→"Binding/Path"，dominant-file 与 central-symbol 双规则跳过测试路径）。**django**：`__init__.py` 重导出枢纽不再命名 2900 符号模块（dunder stem 过滤）；`production_dir_prefix` 让跨 `django/**`+`tests/**` 社区报生产前缀而非空串；SCIP 增量门控（`(path,len,mtime)` 摘要 sidecar，未变则 `up-to-date` 复用——重索引 7:46→2:41，scip-python 4 分钟/3.9GB 只付一次）。**gson**：Maven/Gradle `src/{main,test}/{java,kotlin}` 链 + JVM 反域名包前缀穿透（模块 2 个"Main/Src"→11 个真实业务名 TypeToken/JsonReader/Internal…）；Java 隐式入口（JVM `main`、JUnit `test*`、`Object` 契约 toString/equals/hashCode）；auxiliary 路径（examples/demos/tools/benchmarks）符号视为入口而非死码候选（死候选 193→110，examples 误报 0）。**TypeScript 仓**：结构索引 1228s→52s（23.6×）——rayon 分块并行解析 + 单文件解析预算（默认 2s，`GROUNDGRAPH_PARSE_BUDGET_MS` 可调；tree-sitter 错误恢复在故意非法 fixture 上的病态耗时只跳过 3 个文件，186k 节点/172k 边无损）。自举：rust SCIP up-to-date 生效（重索引 4.4s），check 0 错误，回归 1071/0，clippy 0 警告。*
*本版新增：§0.5 两二进制真实互索引实测、§0.6 可行性结论、§0.7 本轮 TDD 修复、§7 命名占用核验（Morse/Dart、Panelly/Swift 非侵入扫描，0 写回源仓）。*
*P21 更新：tree-sitter 广度后端首语言 Rust 落地，GroundGraph 自举缺口已堵（§0.5 表 + §5.1 + §6 Phase B）；测试对标 SQLite 起步（proptest 任意输入不 panic + 确定性 + 良构 + 5000 层深嵌套不爆栈 + 自举回归）。*
*P22 更新：tree-sitter 广度后端收敛为通用 `LangSpec` 数据驱动驱动并推广到 Rust/TS/Python/Go/C/C++ 六语言（§0.5 结论 2、§0.6、§1.1、§2、§5.1「Tier 2 框架是什么 + 备选取舍」、§6 Phase B）；单个 `treesitter:` 配置开关多语言索引，逐语言 proptest + 端到端多语言集成测试 `p22_treesitter_multilang`。*
