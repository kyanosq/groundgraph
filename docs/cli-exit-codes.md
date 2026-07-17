# CLI 退出码契约（issues.md #233）

`groundgraph` 的进程退出码遵循一套小而明确的契约，让 CI 脚本与 shell 管道能区分
"调用方式写错了"与"程序内部出问题"。

| 退出码 | 语义       | 触发场景                                                                 |
| ------ | ---------- | ------------------------------------------------------------------------ |
| `0`    | 成功       | 命令正常完成（含 `check` 0 findings、`candidate show` 命中）。           |
| `2`    | 用户错误   | 参数非法（clap 解析错误，含 `--format` 取值不合法；以及 runner 运行时参数校验：缺参数 / 互斥参数（如 `search --code` 与 `--file`）/ 非法 `--kind`·`--purity`·字面量类型 / `search --file` 缺 `--line` / `install` 读到非 object 的 JSON config——统一经 `bail_user!` 抛出）、无 `.groundgraph` 工作区（先 `groundgraph init`）、请求的符号/文件未索引、配置不可解析、`check` 发现 error、`doctor` 发现 ✗、`index` 部分失败（某 indexer 缺工具）。用户改变调用或环境即可纠正。 |
| `70`   | 内部错误   | 不可预期的失败（`EX_SOFTWARE`，源自 sysexits.h）：SQLite 层损坏、IO 故障、子进程崩溃、未分类的 anyhow 路径。用户改参数无法解决，应作为 bug 上报。 |

## 设计要点

- **2 与 clap 对齐**：clap 自身的解析错误本来就退出 2，所以"用户输入错"无论被 clap 还是
  被 runner 捕获，退出码都是 2，行为统一。
- **`bail_user!` 宏**：runner 内**参数校验**的统一入口（`exit_code.rs`）。`bail_user!("…")`
  等价于 `return Err(UserError(…))`，其错误经 `classify` 映射到 exit 2——它是 `anyhow::bail!`
  的对称 counterpart：`bail_user!` 表达"用户改调用即可修"（exit 2），`anyhow::bail!` / `?` +
  `.context()` 传播表达"操作性 / 内部故障"（exit 70，除非 cause 链里的 `EngineError` 重新分类）。
  缺参数、互斥参数、非法 `--kind`/`--purity` 值、`install` 非 object JSON config 等都用 `bail_user!`。
- **分类集中在一处**：runner 继续返回 `anyhow::Result`，错误→退出码的映射全部在
  `crates/groundgraph-cli/src/exit_code.rs::classify` 完成，便于审计。它优先级为：
  1. CLI 层显式 `UserError`（runner 检查一个**成功**的结果后决定仍需非零退出，如部分索引失败 / doctor 发现问题）；
  2. cause 链中的 `EngineError`（按 `EngineError::kind()`：`UserInput`/`NotFound` → 2，`Operational`/`Internal` → 70）；
  3. cause 链中的裸 `io::ErrorKind::NotFound`（runner 读了用户指定但不存在的文件）→ 2；
  4. 其余 → 70。
- **引擎层已分类**：`groundgraph-engine::error::EngineError`（#166）的 `kind()` 是本契约消费的
  接缝，把细粒度变体折叠成"修调用 / 修环境 / 报 bug"三档。

## 兼容性

- `candidate show <不存在的 id>` 历史上退出 2，新契约下保持 2（语义即 `NotFound`）。
- 历史上"几乎全 1"的散落错误路径，现在按上面的契约分流为 2 或 70；只断言"非零"的脚本
  不受影响，显式断言 `==1` 的脚本需改为 `!=0` 或按新语义判断。
