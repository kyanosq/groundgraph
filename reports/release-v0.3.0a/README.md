# v0.3.0-A 真实仓库行为对照（4 个仓 / Phase 2 + Phase 3 / 2026-05-23）

> 数据源：`reports/release-v0.3.0a/<repo>/dead-code-low.json` 与
> `reports/release-v0.3.0a/<repo>/search-*.json`，由
> `./target/release/groundgraph --repo-root release-scans/_scratch/<repo>` 直跑生成。
> 四个 scratch 副本沿用 v0.2.0 收口阶段的 graph.db，**目标仓零侵入**。

## A. 死代码 only-low-tier-inbound reason

| 仓库 | 候选总数 | 触发 reason 的候选数 | warnings |
|------|---------|----------------------|----------|
| pixcraft-app | 777 | 0 | 0 |
| atagent | 1038 | 0 | 0 |
| pixcraft-landing | 95 | 0 | 0 |
| vub | 15045 | 0 | 0 |

**解读**：四个仓的 only-low-tier-inbound 命中都是 0。这与
`crates/groundgraph-engine/src/edge_confidence.rs:174` 的现实一致 ——
`*_ast` indexer 出的边落到 `EdgeConfidence::Medium`，`Low` 只留给
AI-derived / overridden / ignored 三类罕见情况。Phase 2 的 reason
加得到位但不会误报，等到后续接 AI derive 时会自然变得有意义。

## B. Search Pass A（evidence boost）+ Pass B（neighbor boost）

| 仓库 | 查询 | 命中总数 | evidence-boost 命中 | neighbor-boost 命中 | warnings |
|------|------|---------|---------------------|---------------------|----------|
| pixcraft-app | `build (--kind dart_method)` | 100 | 75 | 42 | 0 |
| atagent | `create (--kind python_function,python_method)` | 68 | 0 | 0 | 0 |
| pixcraft-landing | `render (no --kind filter; TS/Java kind 别名为 P20 遗留 bug)` | 10 | 0 | 2 | 0 |
| vub | `save (no --kind filter; TS/Java kind 别名为 P20 遗留 bug)` | 100 | 0 | 0 | 0 |

**Pass A 样例（pixcraft-app / build）**：

- `dart_method::lib/core/router/app_router.dart#ScaffoldWithNavBar.build` score=**130**
  - name exactly matches `build`
  - 出边 evidence_quality=high (1 条)，符号有强证据支撑
  - 邻接其他命中（_buildNavItem）

- `dart_method::lib/features/converter/convert_media_screen.dart#_ConvertMediaScreenState.build` score=**130**
  - name exactly matches `build`
  - 出边 evidence_quality=high (4 条)，符号有强证据支撑
  - 邻接其他命中（_buildSettings）

- `dart_method::lib/features/editor/editor_screen.dart#_EditorScreenState.build` score=**130**
  - name exactly matches `build`
  - 出边 evidence_quality=high (9 条)，符号有强证据支撑
  - 邻接其他命中（_buildDrawingTools、_buildEditDraggableSheet 等）

**Pass B 样例（vub / save）**：

- `save` 在 vub 里命中分散在多个 package 中，没有触发邻接 boost；
  这正是 Phase 3 设计意图 —— 邻接加权只在真实 cluster 出现时给出 tie-break 信号。

**Pass B 备份样例（vub / service，邻接 cluster 触发率 30/30）**：

- `file::vhub-boot/src/main/java/com/vhub/boot/service/Constants.java` score=**80**
  - path contains segment `service`
  - 邻接其他命中（com.vhub.boot.service）

- `file::vhub-task/src/main/java/com/vhub/task/service/ScheduleJobLogService.java` score=**80**
  - path contains segment `service`
  - 邻接其他命中（com.vhub.task.service）

- `file::vhub-task/src/main/java/com/vhub/task/service/impl/ScheduleJobLogServiceImpl.java` score=**80**
  - path contains segment `service`
  - 邻接其他命中（com.vhub.task.service.impl）

## C. 已知遗留 bug（不属于 v0.3.0-A 引入）

- `groundgraph-cli/src/commands/search.rs::parse_kind` 的 P20 补丁只补了
  Dart / Swift / Go / Python 的别名表，**TypeScript / Java NodeKind**
  仍然不在 match 中，所以 `--kind typescript_function` / `--kind java_method`
  会被 CLI 自身的别名解析器以 `unknown --kind` 拒绝，尽管 engine 的
  `default_search_kinds()` 已经把它们列为 valid。
- 影响：本报告里 pixcraft-landing / vub 的搜索查询无法按 kind 过滤，
  改为不带 --kind 直跑（命中里包含 file / module 类型），这反而更
  真实地展示了 Pass B 邻接加权在 file 级的 cluster 行为。
- 处置：v0.3.0-A 不引入此 bug，也不在本阶段修。后续在 v0.3.0-B 或
  P20 follow-up 里把 TS / Java kind 加进 `parse_kind`，附别名。

## D. 复现方式

```bash
cargo build -p groundgraph-cli --release
for repo in pixcraft-app atagent pixcraft-landing vub; do
  ./target/release/groundgraph --repo-root release-scans/_scratch/$repo \
    dead-code --json --min-confidence low \
    > reports/release-v0.3.0a/$repo/dead-code-low.json
done
./target/release/groundgraph --repo-root release-scans/_scratch/pixcraft-app \
  search --kind dart_method --json --limit 100 build \
  > reports/release-v0.3.0a/pixcraft-app/search-build.json
# ... etc, see scripts/release_scan_v030a_metrics.py for the full matrix
python3 scripts/release_scan_v030a_metrics.py > reports/release-v0.3.0a/README.md
```
