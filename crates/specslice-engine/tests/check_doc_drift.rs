//! Doc→code drift checks（“根据文档查找代码实现疏漏/错误实现”的第一层）。
//!
//! 2026-06 中立评审实证：`check` 只验证 *声明过的* 链接（links.yaml /
//! requirements md），对文档正文与代码的真实漂移完全沉默——自仓跑出 0 findings。
//! 本测试钉住升级后的行为：
//!
//! 1. `doc_stale_code_ref` — 文档行内反引号引用了不存在的仓库路径，或
//!    “容器存在但成员不存在”的符号（`Engine::not_real()`）→ 文档过期或实现
//!    疏漏。外部 crate 引用（`rusqlite::Connection`）与围栏代码块内的示例
//!    路径不许误报（精度优先）。
//! 2. `requirement_implementation_hint` — 孤儿需求自动在图上找疑似实现：
//!    找到 → 列出候选并提示 `connect`；找不到 → 提示疑似实现缺失。

use std::path::{Path, PathBuf};

use specslice_engine::checks::{run_checks, CheckOptions};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions};
use specslice_engine::fulltext_indexer::rebuild_fulltext_index;
use specslice_engine::init::{init_repository, InitOptions};
use specslice_engine::requirements_md_indexer::{
    index_requirements_md, RequirementsMdIndexOptions,
};
use specslice_engine::{index_rust, RustIndexOptions};
use specslice_store::Store;

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "src/engine.rs",
        r#"pub struct Engine;

impl Engine {
    pub fn start(&self) -> u32 {
        // Platform-API call site: the method is *used* here, not defined.
        let lum = color.computeLuminance();
        42
    }
}
"#,
    );
    write(
        root,
        "src/core/util.rs",
        "pub fn helper() -> u8 {\n    1\n}\n",
    );
    write(root, "Cargo.toml", "[package]\nname = \"fixture\"\n");
    // `legacy/` exists on disk (so the first-segment rule keeps it in scope)
    // but is suppressed via the `doc_drift_ignore` config below.
    write(root, "legacy/keep.rs", "pub fn old() {}\n");
    // `tool/` is real but outside the indexed code roots — bare references to
    // its files must resolve against the working tree, not just the graph.
    write(root, "tool/helper.dart", "void helper() {}\n");
    write(
        root,
        "docs/design.md",
        r#"# 模块设计

入口在 `src/engine.rs`，核心是 `Engine::start()`。
下面这两个引用已经漂移：`src/missing_file.rs` 与 `Engine::not_real()`。
重复引用去重验证：`src/missing_file.rs` 再次出现。
外部依赖 `rusqlite::Connection` 不属于本仓，不应报告。

裸文件名引用：`engine.rs` 存在（按 basename 解析），`vanished_module.rs` 不存在。
产物文件名 `index.scip` 与 `graph.db` 不是源码引用，不应报告。
枚举成员 `Engine::Started` 这类大写成员未被索引，不应报告。
纯扩展名 `.ts` 与花式写法 `.js↔.ts` 不是引用，不应报告。
根级文件 `Cargo.toml` 虽未被索引为节点，但磁盘上存在，不应报告。
资源/配置类裸文件名歧义大：`links.yaml`、`schema.sql`、`CHANGELOG.md`、`go.mod` 不应报告。
无括号的成员引用可能是字段/配置键：`Engine.missing_field` 与 `Engine::missing_assoc` 不应报告。
含占位符的路径 `reports/<name>/out.rs` 不应报告。
brace 展开 `src/{engine,index}.rs` 与省略号 `docs/.../design.md` 是简写，不应报告。
小写变量上的方法调用 `path.is_file()` 是代码叙述，不应报告。
占位样例名 `foo.rs` 与 `lib/foo.rs` 是教学示例，不应报告。
谈论其他仓库形态的路径 `pages/api/handler.ts`（本仓没有 `pages/` 这一首段目录）不应报告。
被配置忽略的历史目录 `legacy/gone.rs` 不应报告。
未被索引但真实存在于工作树的 `helper.dart` 不应报告。
C/C++ 裸头文件名 `archive.h` 与 `zlib.h` 通常指系统/第三方头，不应报告。
相对上跳路径 `../sibling/README.md` 相对的是文档自身而非仓库根，不应报告。
模板化路径 `docs/round-XX-report-YYYY-MM-DD.md` 含日期/序号占位，不应报告。
平台/标准库 API 的裸调用 `click()`、`focus()`、`pop()`、`compute()` 是单词通用名，不应报告。
多词裸调用 `engine_start_helper()` 信息量足够，图中与源码内容都没有时应当报告。
多词平台 API `computeLuminance()` 虽无定义节点，但源码体内有调用点，不应报告。
省略前缀的路径 `core/util.rs` 能按后缀对齐到已索引的 `src/core/util.rs`，不应报告。
SpecSlice 运行时产物 `.specslice/candidates/business_logic.yaml` 不应报告。

```text
围栏代码块里的 `src/fake_in_fence.rs` 是示例，不应报告。
```
"#,
    );
    write(
        root,
        ".specslice/requirements/reqs.md",
        r#"# REQ-001 engine start flow

引擎启动流程。

# REQ-002 quantum teleport scheduling

完全没有对应代码的需求。
"#,
    );

    init_repository(InitOptions::new(root)).unwrap();
    let cfg_path = root.join(".specslice.yaml");
    let mut cfg: specslice_engine::config::EngineConfig =
        serde_yaml::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
    cfg.checks.doc_drift_ignore = vec!["legacy/**".into()];
    std::fs::write(&cfg_path, serde_yaml::to_string(&cfg).unwrap()).unwrap();
    let mut store = Store::open(root.join(".specslice/graph.db")).unwrap();
    store.migrate().unwrap();
    index_rust(
        &mut store,
        &RustIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        },
    )
    .unwrap();
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: root.to_path_buf(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec!["**/*.md".into()],
        },
    )
    .unwrap();
    index_requirements_md(
        &mut store,
        &RequirementsMdIndexOptions {
            repo_root: root.to_path_buf(),
            requirements_dir: PathBuf::from(".specslice/requirements"),
        },
    )
    .unwrap();
    rebuild_fulltext_index(&mut store, root).unwrap();
    tmp
}

#[test]
fn stale_doc_refs_are_reported_with_precision() {
    let tmp = fixture();
    let report = run_checks(CheckOptions {
        repo_root: tmp.path().to_path_buf(),
        impact: None,
    })
    .unwrap();

    let stale: Vec<&str> = report
        .findings
        .iter()
        .filter(|f| f.code == "doc_stale_code_ref")
        .map(|f| f.message.as_str())
        .collect();

    assert!(
        stale.iter().any(|m| m.contains("src/missing_file.rs")),
        "missing path must be reported, got {stale:?}"
    );
    assert!(
        stale.iter().any(|m| m.contains("Engine::not_real")),
        "container-exists-member-missing symbol must be reported, got {stale:?}"
    );
    // Precision guards: existing refs, external crates, fenced examples.
    assert!(
        !stale.iter().any(|m| m.contains("src/engine.rs")),
        "existing path must NOT be flagged: {stale:?}"
    );
    assert!(
        !stale.iter().any(|m| m.contains("Engine::start")),
        "resolvable symbol must NOT be flagged: {stale:?}"
    );
    assert!(
        !stale.iter().any(|m| m.contains("rusqlite")),
        "external crate ref must NOT be flagged: {stale:?}"
    );
    assert!(
        !stale.iter().any(|m| m.contains("fake_in_fence")),
        "fenced example must NOT be flagged: {stale:?}"
    );

    // Bare-filename refs resolve by basename against indexed paths.
    assert!(
        stale.iter().any(|m| m.contains("vanished_module.rs")),
        "missing bare filename must be reported, got {stale:?}"
    );
    assert!(
        !stale.iter().any(|m| m.contains("`engine.rs`")),
        "existing bare filename must NOT be flagged: {stale:?}"
    );
    // Build-artifact filenames and unindexed member shapes are skipped.
    assert!(
        !stale
            .iter()
            .any(|m| m.contains("index.scip") || m.contains("graph.db")),
        "artifact filenames must NOT be flagged: {stale:?}"
    );
    assert!(
        !stale.iter().any(|m| m.contains("Engine::Started")),
        "uppercase members (enum variants / assoc consts) are not indexed → must NOT be flagged: {stale:?}"
    );
    // The same drifted ref mentioned twice in one document reports once.
    assert_eq!(
        stale
            .iter()
            .filter(|m| m.contains("src/missing_file.rs"))
            .count(),
        1,
        "duplicate refs must be deduped, got {stale:?}"
    );

    // Noise guards discovered by dogfooding the real repo (371→…):
    for benign in [
        "`.ts`",
        "`.js↔.ts`",
        "Cargo.toml",
        "core/util.rs",
        ".specslice/candidates/business_logic.yaml",
        "links.yaml",
        "schema.sql",
        "CHANGELOG.md",
        "go.mod",
        "Engine.missing_field",
        "Engine::missing_assoc",
        "reports/<name>/out.rs",
        "src/{engine,index}.rs",
        "docs/.../design.md",
        "path.is_file()",
        "foo.rs",
        "pages/api/handler.ts",
        "legacy/gone.rs",
        "helper.dart",
        "archive.h",
        "zlib.h",
        "../sibling/README.md",
        "round-XX-report-YYYY-MM-DD.md",
        "click()",
        "focus()",
        "pop()",
        "compute()",
        "computeLuminance",
    ] {
        assert!(
            !stale.iter().any(|m| m.contains(benign)),
            "benign ref {benign} must NOT be flagged: {stale:?}"
        );
    }
    // Multi-word bare calls carry enough signal to verify.
    assert!(
        stale.iter().any(|m| m.contains("engine_start_helper")),
        "missing multi-word bare call must be reported, got {stale:?}"
    );
}

#[test]
fn orphan_requirements_get_implementation_hints_from_the_graph() {
    let tmp = fixture();
    let report = run_checks(CheckOptions {
        repo_root: tmp.path().to_path_buf(),
        impact: None,
    })
    .unwrap();

    let hints: Vec<&str> = report
        .findings
        .iter()
        .filter(|f| f.code == "requirement_implementation_hint")
        .map(|f| f.message.as_str())
        .collect();

    // REQ-001's title tokens match `Engine::start` — the graph suggests it.
    assert!(
        hints
            .iter()
            .any(|m| m.contains("REQ-001") && m.contains("start")),
        "REQ-001 must get a plausible-implementation hint, got {hints:?}"
    );
    // REQ-002 matches nothing — likely a real implementation gap.
    assert!(
        hints
            .iter()
            .any(|m| m.contains("REQ-002") && (m.contains("未找到") || m.contains("疏漏"))),
        "REQ-002 must be called out as likely unimplemented, got {hints:?}"
    );
}
