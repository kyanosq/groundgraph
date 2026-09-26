# Java 迁移结构层

本轮补上调用点清单、编译期类型绑定、重载身份、继承候选、Feign 静态服务关联，以及迁移门和页面出口。Java 普通调用不再经过通用方法名匹配器。

## 两层分析，一份调用清单

Tree-sitter 负责结构与调用清单。每次方法调用、构造、显式构造委托和方法引用都记录文件、字节区间、行列、表达式和所属符号；重复调用、共享起始位置的链式调用、递归不会折叠。语法错误明确提示清单可能不完整。字段初始化也在清单中，归属类型或文件。

JDK 的 `JavacTask.parse/analyze` 与 `Trees.getElement` 负责声明类型、同包/导入、重载、泛型、继承和静态目标绑定。直接使用 JDK API，没有增加 Java 运行库依赖，不调用 Maven/Gradle，不生成 class，不执行客户业务代码；注解处理器关闭。JDK 11+ 可启动适配器，待分析的语言特性还必须被该 JDK 支持。调用位置按 UTF-8 字节对齐，包括非 ASCII 文本。

编译器缺失、超时、源码缺依赖、出错类型、无法映射的声明均保留为 `unresolved`，说明原因。编译器识别到的第三方/JDK 方法记录 `external_target`，不伪造本仓库节点。已解析仅表示**编译期目标**；接口实现关系仍为候选，不等于运行时唯一实例。

调用记录存入索引文件节点的 `java_analysis` 元数据，是唯一权威记录。trace、feature-pack、graph 与页面从这里投影；关系边保留调用点 ID。SCIP 部分覆盖不能按文件删除 javac 或 Feign 关系。重建索引会清理旧代关系。

## 配置

默认尝试 PATH 中的 `java`。没有可用编译器时仍输出调用清单和显式部分失败；`index` 默认 `--fail-on-partial=true`，此时以非零状态结束。排查时可显式传 `--fail-on-partial=false` 保留部分结果，但迁移门仍检查未解析调用。

```yaml
java_semantics:
  enabled: true
  java_command: java
  timeout_seconds: 120
  classpath: [] # 已有依赖 jar；不自动下载或运行依赖构建
  source_sets:
    shared:
      roots: [shared/src/main/java]
    order:
      roots: [order/src/main/java]
      dependencies: [shared]
      classpath: []
    inventory:
      roots: [inventory/src/main/java]
      dependencies: [shared]
  services:
    inventory-service: [inventory/src/main/java]
```

每个文件只能归属一个 source set。依赖集合决定编译可见范围，允许合法跨模块调用，拒绝通过全仓同名匹配跨越边界。未配置 source sets 且发现多个 Maven/Gradle 构建模块时，不猜测可见性，保留未解析及配置诊断。一次编译失败不丢弃其他集合的结果。`services` 是显式部署服务映射，不从 Java 包名猜测。

## Feign 与数据层

解析 `@FeignClient`、Controller 与 Request/Get/Post/Put/Delete/PatchMapping，组合类级前缀、客户端前缀、方法级路径和 HTTP 方法，按配置的服务根定位服务端。路径参数名不同可作为候选匹配；多个匹配均保留。未配置服务、没有端点、动态占位符、无法确定的常量、URL 覆盖等均留诊断。动态网关重写、条件 Bean 和实际部署版本仍需外部证据。

`Elements.overrides` 建立真实覆盖关系，包括传递继承、多实现及泛型契约；不再使用 `Impl` 命名或只按方法名连实现。MyBatis XML 使用完整、大小写敏感的 namespace 加方法名关联；有多个重载时只给候选，不跨包误连。

## 使用与验收

```sh
groundgraph index
groundgraph trace 'OrderService.submit(Order)' --json
groundgraph feature-pack --path order/src/main/java
groundgraph graph --format web --out graph.html
```

页面的“调用点”页可搜索表达式/路径/目标，筛选未解析项并定位调用者。未解析调用同时进入“待核查”。导出局部范围时保留相应调用记录。

迁移工作包新增 `call_dispositions`：每个未解析调用必须恰好有一项处置。`excluded` 要写理由；`mapped/platform` 还必须绑定目标代码和独立回归检查。缺项、重复项、未知 ID 或仍未解析均阻止检查门通过。测试通过仍不等于自动证明全业务等价。

**兼容性：Java 方法/构造器 ID 现在带参数类型签名，零参数带 `()`。必须重新索引并重新核对旧迁移映射，不能按名称自动批准旧映射。** 源码区间 ID 只保证同一快照重复索引稳定；源码编辑后应重新生成工作包并校验哈希。

方法引用保留编译期目标，但导出 `references`，不冒充已经执行的 `calls`。Feign 的继承接口映射、组合注解及 params/headers/consumes 条件尚未完整建模；已有静态匹配只作候选。

动态反射、运行时 Bean 选择、生成代码及缺失的私有依赖不能凭静态源码完整还原。尚未提供这些证据的结果保持未知；注解 SQL、MyBatis-Plus 表语义及非 HTTP 入口不属于本轮新增支持。

## 回归

`cargo test -p groundgraph-engine --test java_semantics --locked` 覆盖重载、重复调用、链式调用、递归、缺编译器、缺依赖、泛型、传递继承、多实现、Feign 服务/方法隔离、mapper namespace、模块可见性、UTF-8、JUnit、初始化与 varargs、导出、部分 SCIP 保留。

`python3 -m unittest discover -s scripts -p test_migration.py` 覆盖未解析调用处置门；`cd webui && npm test && npm run test:browser` 覆盖调用清单的投影、过滤和定位。

本轮实跑：820 项引擎单测、9 项 Java 集成、65 项存储测试、87 项 CLI 单测、41 项代码事实/多语言集成、9 项 CLI 证据/资源集成、12 项迁移门测试及 6 项前端模型测试全部通过；浏览器操作测试、clippy（拒绝警告）、rustdoc（拒绝警告）、格式和资源同步检查通过。另一次完整 workspace 测试因 Dart sidecar 长时间等待而主动停止，不列为全量通过。

脱敏 OMS 本地验证：5,885 个 Java 文件、15 个显式源集；使用 JDK 11.0.18、debug 二进制，未补外部/私有依赖，服务短名到模块的映射仅用于静态候选验证。最终索引用时 161.18 秒、退出 0；默认 500ms 解析预算曾跳过大型订单实现文件并返回 2，保留失败记录后以 `GROUNDGRAPH_PARSE_BUDGET_MS=3000` 重跑完成，没有关闭部分失败检查。

| 本地语料结果 | 数量 |
|---|---:|
| 保留的调用/构造/方法引用位置 | 227,335 |
| 编译期已解析 | 57,112 |
| 明确保留未解析 | 170,223 |
| 本仓库静态调用边 / 方法引用边 | 20,924 / 104 |
| 实现分派候选边 | 3,813 |
| Feign 候选边 | 1,265 |
| Feign 无匹配诊断 / 动态配置诊断 | 12 / 1 |

这些是清单和候选数量，不是准确率、召回率或迁移完成率。缺失依赖、生成代码和源码绑定限制仍造成大量未知；只有补齐对应构建上下文并复验，才能减少这些缺口。源码、服务映射、工作包、日志和二进制哈希全部留在本地验证目录，不进入公开仓库。

官方 API：[JavacTask](https://docs.oracle.com/en/java/javase/17/docs/api/jdk.compiler/com/sun/source/util/JavacTask.html)、[Trees](https://docs.oracle.com/en/java/javase/17/docs/api/jdk.compiler/com/sun/source/util/Trees.html)。
