# GroundGraph 设计审查与本轮修复

审查范围：索引生成与事务、图断言及导出、源码读取、迁移证据门、离线可视化。基线为 9a7a6e0；本轮也纳入此前尚未提交的迁移证据修改。结论是保留现有图存储、索引和检索能力，收敛重复实现，修正可信性；不能把当前版本描述为完整 Java 语义迁移器。

后续更新：本文保留原审查基线。逐调用点清单、javac 编译期绑定和 Feign 声明候选现已补充，最新支持范围及验证见 [Java 结构层](java-semantics.md)；运行时分派与完整框架语义仍未宣称完成。

## 已修复的根因

| 问题 | 修复及回归依据 |
|---|---|
| CLI 提交代码索引之后才单独写 schema，watch/library 与 CLI 的图不同，失败留下混合代数据 | schema 纳入引擎同一事务，先于全文重建；独立 schema 更新也使用事务。真实索引后破坏 SQL 编码，验证所有旧节点原样保留。 |
| schema 扫描悄悄忽略遍历/读取错误 | 传播实际 IO 错误；容量跳过计入 partial failures，CLI 严格模式阻塞。 |
| 启发式调用仍标 fact/confirmed/1.0 | 共用 reference ingestion helper 标 candidate/proposed，证据写明解析限制；直接 implements 的方法配对也是候选。当前尚无逐调用点 unresolved 清单。 |
| 数据库保留旧 certainty/confidence，却让较弱断言覆盖证据、状态 | 单条和 bulk UPSERT 使用同一整体替换条件；证据与断言等级不可拆开更新。回归覆盖两个写入口。 |
| Java 只靠 Impl 命名建立实现桥 | 从真实 Java implements、包/导入定位接口；不再猜命名。仍不完整支持 extends、重载和 DI 分派。 |
| 普通 Java 调用未经语义验证却被判为纯函数 | 复用 Java AST 扫描；存在未验证调用或抽象声明时返回 unknown，已检测到副作用仍为 impure。pure 仍是局部静态信号，不是程序纯度证明。 |
| 原始网络导出合并丢失断言、删除自递归和孤立节点 | 按关系分组但保留所有断言；递归保留；缺失端点单列，显示数量与限制。 |
| Python/Rust 两个导出器与 HTML/Web 两套页面各自解释同一图 | 删除 Python exporter、旧 HTML renderer 和 3D bundle，两个 CLI 格式共用一个 workspace 和数据适配器。模板缺失/重复标记、坏 JSON 均失败，不做静默兜底。 |
| web 格式悄悄忽略 focus/view/max-nodes | 显式筛选走已有 curated view 管线；无筛选仍保留全量 raw network。 |
| 配置错误时统计/SCIP 改用默认目录，可能误删另一份统计 | 缺少 workspace 的 pre-init 行为保留；配置存在但错误时向调用方返回错误，不换目录。可选 telemetry 失败显式记录且不改变业务命令结果。 |
| 图中的路径或 symlink 可从仓库外读取源码 | 源码跨度、全文重建与 MCP 共用 canonical 路径限制；MCP 实际读取错误传播，不再 `.ok()` 吞掉。 |
| 汇总失败的 JUnit 被当作通过；旧报告可套在新映射上 | 检查 suite 失败/错误/跳过/disabled 计数；报告绑定原始 manifest SHA-256，测试后复核文件与 manifest，页面拒绝过期或无绑定报告。 |

## 对独立评估的判断

“需要修复可信性再用于迁移结构层”的方向成立。旧版调用图的高召回不能抵消误连；以调用者+方法名聚合、仅计算双方都已经进入图的边，其召回也不是所有调用点的覆盖率。缺依赖时的 javac 归因是很有价值的独立参照，但仍需把 unresolved、生成成员、异常跳过和近似框架闭包单列，不能称作运行时真值。

优先级应调整：

1. **身份与证据先行**：逐调用点清单、解析状态、重载签名 ID、MyBatis namespace FQN。身份碰撞会让后续更准确的解析结果互相覆盖，因此不能把重载 ID 排到末尾。
2. **Java 类型解析作为独立适配器**：接收者类型、同包、模块依赖、继承和重载。采用成熟前端，避免在 tree-sitter 名称匹配上重造类型系统。JDT binding recovery 和 Spoon noclasspath 都可能返回不完整绑定，未知必须继续保留。[JDT API](https://help.eclipse.org/latest/ntopic/org.eclipse.jdt.doc.isv/reference/api/org/eclipse/jdt/core/dom/ASTParser.html)、[Spoon Environment](https://spoon.gforge.inria.fr/mvnsites/spoon-core/apidocs/spoon/compiler/Environment.html)。
3. **按实际迁移链路补框架**：Feign、BaseMapper/ServiceImpl、注解 SQL、定时与事件入口。服务名+路径是静态候选匹配，不能当作运行时调用成功或唯一目标。具体项目没用到的 MQ/调度框架不提前实现。
4. **三轴独立**：范围处置、目标实现证据、验证结果分别记录。现有工作包是文件式证据门，不是具备权限/签名/完整变更历史的审批系统；不另建一套与 KTOS 重复的迁移业务平台。

两处建议不能照单实现：接口/调用只限“同 Maven 模块”会漏合法跨模块依赖，应基于依赖可见性；同名表全局合并会把不同数据库/租户的表混为一体，应使用数据源+schema+表名身份。SQL 动态表名、动态 Bean、反射等保留明确未知，不用补假边消除缺口。

## 尚未闭环

逐调用点 unresolved、Java 接收者/重载/传递继承/运行时 DI、Feign 跨服务、非 HTTP 入口、注解 SQL / MyBatis-Plus、完整 DTO 和常量值解析，以及独立业务验收均未完成。当前版本可用于有范围、有人工复核的迁移工作包；不能作为自动证明“源行为全部迁完”的放行器。新增 candidate/proposed 枚举需要新版读取端，旧缓存须重新索引；没有改系统中已安装的旧 CLI。

数据库 PRAGMA/提交后的 checkpoint 等 best-effort 清理不能一概判为“吞异常导致成功”：提交已经成功时再报告整次写入失败反而诱发重试。保留已有事务恢复策略，重点修复影响数据完整性却被隐藏的失败，不追求机械删除所有 `.ok()`。

## 本地验证结果

| 检查 | 实际结果 |
|---|---|
| `cargo test -p groundgraph-engine --tests --locked` | 1027 通过，3 项按原设置忽略（外部工具/语料及 fixture 再生成）。 |
| CLI binary、`migration_evidence`、`webui_assets` | 96 通过；首次 workspace 运行中的其余 CLI 测试也通过。 |
| store / MCP / lang-dart 测试 | 分别 65 / 62 / 66 通过。 |
| workspace doctests、fmt、Clippy `-D warnings`、Rustdoc `-D warnings` | 通过。 |
| Python 迁移门、Node 数据适配器 | 9 / 5 通过。 |
| Playwright 页面回归 | 桌面与窄屏、关系证据、坏文件保留现有数据、报告绑定和过期拒绝均通过。 |
| 资源同步、单元测试覆盖基线、npm audit | 通过；npm 无已知漏洞。 |

首次 `cargo test --workspace --locked` 在两项仍要求启发式调用为 fact 的旧断言处停止；更新断言后完整重跑 engine，并复核以上受影响包，没有把首次失败的命令记为通过。Rust core 在首次运行中通过。

本地 OMS 工作包在 HTTP 预览中实际打开，保留未通过的迁移门结果。CLI 单文件 HTML 的内嵌资源和转义检查通过，但内置浏览器不允许 `file://`，因此没有把直接文件打开记为浏览器验收通过。测试通过的范围不扩大为旧 OMS 运行或业务迁移验收。
