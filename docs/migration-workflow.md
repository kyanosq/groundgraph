# 源码迁移工作流

GroundGraph 提供源码定位、依赖线索和工作包，迁移后的业务系统仍通过自己的契约执行。
代码图既不转译整个应用，也不证明行为等价。名称与结构比较只用于查漏。

## 生成工作包

```sh
cargo build --locked -p groundgraph-cli
python3 scripts/migration.py prepare \
  --groundgraph ./target/debug/groundgraph \
  --source-root /path/to/old-system \
  --source-file src/Price.java \
  --source-file src/PriceRequest.java \
  --out /path/to/local-work/price-v1
```

只读取明确选定的源码文件；XML 必须是 MyBatis mapper。它把源码复制到隔离的本地工作目录后索引，禁用自动外部语义工具。选定切片没有原项目完整模块可见性，因此只保留 Java 调用清单，不猜测编译上下文；完整 javac 绑定在配置好 source sets 的原项目索引中执行。输出：

- `source/`：选定源码的快照及可重建的图缓存。
- `feature-pack.json`：源码行号、符号、关系、检查线索和关系的完整 assertion。
- `migration.json`：原文件 SHA-256、图证据 SHA-256 和待处置清单。
- `index.log`：索引输出；部分索引失败时命令失败。

现有工作目录不会被覆盖。更新源代码时另建快照，再审查映射差异。客户工作包保存在客户/交付工作目录，不提交到本公开仓库。

## 映射与回归

`items` 支持多对多：一项可关联多个源符号和多个目标文件，也可将一个源方法拆为多个有独立 ID 的迁移项。目标路径相对 `--target-root`。

处置值为 `mapped`（包内实现）、`platform`（已有平台机制承担）、`excluded`（范围外，必须说明原因）、`unresolved`（阻塞）。前两种都必须关联目标文件哈希和回归检查。实际项目应补充源方法中的业务检查、状态、权限、事务和副作用审查；符号清单不是自动提取的完整业务规则清单。

每个回归检查的最小格式：

```json
{
  "id": "price-regression",
  "argv": ["pnpm", "exec", "vitest", "run", "test/price.test.ts", "--reporter=junit", "--outputFile={junit}"],
  "files": [{"path": "test/price.test.ts", "sha256": "填入文件 SHA-256"}],
  "oracle": "business_approved",
  "oracle_evidence": "经业务确认的用例集版本或旧系统运行证据位置"
}
```

`oracle` 允许 `legacy_execution` 或 `business_approved` 通过独立预期检查。`code_derived` 可记录，但不能冒充独立验证；仅从同一段代码推导实现和测试可能一起出错。证据说明和 `review` 是人工声明，脚本不验证签名或业务真实性。审核者必须检查这些声明，不能由 AI 自批。

```sh
python3 scripts/migration.py check \
  --manifest /path/to/local-work/price-v1/migration.json \
  --source-root /path/to/old-system \
  --target-root /path/to/new-system \
  --run-tests
```

`--run-tests` 执行 manifest 中的命令；只对开发者已审查的 manifest 使用。命令不经过 shell。
每次检查使用全新的 JUnit 路径；旧报告、零用例、失败、跳过、命令失败、版本漂移都会阻塞。
测试前后复核所有固定的源文件、目标文件和测试文件。测试数据、配置和依赖锁文件也应列入 `files`。
退出码 0 表示记录的范围可进入审查；2 表示阻塞。输出始终写明 `behavioral_equivalence: not_proven`。
测试通过不会自动装包、开放写接口或发布。

## 精度边界

- Java 使用带参数签名的身份、逐调用点清单与 javac 编译期绑定；传递继承和多实现保留候选，Feign 按显式服务映射关联。配置及限制见 [Java 结构层](java-semantics.md)。运行时 Bean、反射和缺失私有依赖仍需独立证据。
- 其他语言普通关系可能含 tree-sitter 启发式结果，标记 candidate/proposed。`feature-pack` 和 `trace` 保留原始置信度、来源、证据、状态和 indexer；不得丢弃这些字段后声称确定调用链。Java 旧 ID/旧快照须重新生成并复核映射。
- 每个未解析 Java 调用都须在 `call_dispositions` 中明确处置；映射到本地或平台的调用须关联目标文件及独立回归检查。旧 Java 工作包缺少调用清单时阻塞，须重建。
- 证据行有上限，依赖可能位于选定范围外；未出现的调用不等于不存在。`review.status=reviewed` 之前必须回看源码并审查依赖边界，不凭图的空白自动排除行为。
- 有可用 Java 构建/classpath 时，可复用现有 SCIP 导入来增强语义；不为了取得语义图重建整个旧生产环境。
- 任意程序等价、并发、外部接口效果、生产数据对账、实际界面流程仍需分别验证。本脚本验证所声明的工作包证据，不代替这些验收。

## KTOS 中的分层

GroundGraph 保持通用，负责图和证据；交付工具/酷小智负责能力分类、AI 转换、迁移映射及回归；KTOS 只安装通过审查的包并执行其公开契约。业务对象、检查项和算价留在包内；ERP、BL 等由连接器接入。此工具不成为 KTOS 运行时依赖，也不把每个函数或 if 变成节点。

验证命令：

```sh
cargo test --locked -p groundgraph-cli --test migration_evidence
python3 -m unittest discover -s scripts -p test_migration.py
```

## 可视化复核

将 check 的 JSON 输出保存为本地报告，在[工作台](../webui/README.md)同时打开 feature-pack.json、migration.json 和报告。报告新增 manifest_sha256，绑定映射原始字节；修改映射后必须重新运行检查。页面不执行测试命令，也不凭映射比例推导通过状态。
