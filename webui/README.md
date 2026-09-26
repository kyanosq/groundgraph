# GroundGraph 代码证据工作台

围绕一个符号检查上下游、关系证据和迁移去向。使用原生 DOM / SVG，无运行时 npm 依赖、CDN、WebGL 或 `eval`。

```sh
cargo build --locked -p groundgraph-cli
./target/debug/groundgraph --repo-root /path/to/repo graph --format web --out /tmp/graph.html
```

`graph.html` 是可离线打开的单文件。`--format html` 使用同一个页面，保留 curated view 的筛选；`--format web` 默认导出全部节点（包括孤立节点）。传入 `--view`、`--focus` 或 `--max-nodes` 时使用同一 curated 筛选管线，不再悄悄忽略参数。

页面支持：

- 搜索名称、路径、完整 ID，按类型筛选；列表分批加载，不删除数据中的孤立节点。
- 选择符号后按上游、下游或双向追踪 1–3 层。图一次最多展示 25 个节点；截断和关闭的关系类型明确显示。大图可滚动，“适应宽度”可缩放到容器。
- 点击关系查看每份原始 assertion 的来源、状态、置信度、证据和解析器。相同端点、相同关系的多份证据并列保留。
- 载入 `feature-pack.json`、`trace --json` 或 `graph --format json`。范围外端点、导出限制和缺少证据的关系列入“待核查”。
- 同时打开 `migration.json` 与 `migration.py check` 的 JSON 报告。报告只有与映射文件的原始字节 SHA-256 匹配才显示有效检查状态；旧版无哈希报告、版本变化、校验不可用均不放行。
- 映射表显示多对多源符号、目标文件、处置理由、回归检查和预期来源。它是只读审查界面，不执行 manifest 内的命令。
- 导出当前图范围用于复核，保留证据与范围限制。

页面中的映射状态、图结构、人工声明均不证明业务等价。`ready_for_review` 只是本地证据门的结果，不是受信任的审批签名。源码片段属于导出时的快照；改动源文件后须重新索引、导出。

开发预览：

```sh
python3 -m http.server 8788 --bind 127.0.0.1 --directory webui
# http://127.0.0.1:8788/
```

启动时不自动加载示例；可点击“浏览内置示例”。`?data=./data/example.json` 仅允许同源文件。客户数据放在自己的交付目录，通过文件选择器读取，不提交到仓库。

`webui/` 是唯一页面源。`scripts/sync_webui_assets.sh` 将四个资源复制进 CLI crate，供 Cargo 打包；`--check` 和 Rust 测试检测副本漂移。旧 Python SQL 导出器和两套独立页面已删除。

验证：

```sh
cd webui
npm ci
npm test
npx playwright install chromium
npm run test:browser
npm audit --audit-level=high
```

浏览器测试使用合成数据，验证局部追踪、孤立节点、多证据、坏文件保留原状态、报告绑定/过期和移动宽度布局，截图保存在忽略提交的 `webui/shots/`。
