//! `groundgraph select-tests` — P19 test selection.
//!
//! Given a git diff (`--base` / `--head`), emit the subset of
//! TestCase / TestGroup nodes that *should* run, each with a
//! list of human-readable reasons and a `high` / `medium` /
//! `low` confidence label.
//!
//! ```text
//! groundgraph select-tests --base main
//! groundgraph select-tests --base main --head HEAD --include-deps
//! groundgraph select-tests --base main --format json
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::test_selection::{
    select_tests, SelectedTest, TestSelection, TestSelectionOptions,
};

#[derive(Debug, Clone)]
pub struct SelectTestsRunArgs {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    pub include_dependent: bool,
    pub max_propagation_depth: usize,
    pub format: String,
}

pub fn run(args: SelectTestsRunArgs) -> Result<()> {
    let report = select_tests(TestSelectionOptions {
        repo_root: args.repo_root,
        base_ref: args.base_ref,
        head_ref: args.head_ref,
        include_dependent: args.include_dependent,
        max_propagation_depth: args.max_propagation_depth,
    })
    .context("computing test selection")?;
    match args.format.as_str() {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).context("serialising test selection")?
            );
        }
        "text" | "" => print_text(&report),
        other => anyhow::bail!("unsupported --format `{other}` (expected text|json)"),
    }
    Ok(())
}

fn print_text(report: &TestSelection) {
    println!("GroundGraph select-tests · {} → {}", report.base, report.head);
    println!(
        "改动文件 {} · 改动符号 {} · 推荐测试 {}",
        report.stats.changed_files, report.stats.changed_symbols, report.stats.tests_selected,
    );
    println!();
    if report.tests.is_empty() {
        println!("(没有需要执行的测试 — 改动可能没有命中任何被代码图覆盖的符号)");
        println!(
            "提示: 这不意味着完全无风险。请先用 `groundgraph impact --base {base}` 查看业务侧影响，再决定是否手动补跑端到端测试。",
            base = report.base
        );
        return;
    }
    let mut last_confidence: Option<&str> = None;
    for test in &report.tests {
        if Some(test.confidence.as_str()) != last_confidence {
            println!();
            println!("== 置信度: {} ==", confidence_label(&test.confidence));
            last_confidence = Some(test.confidence.as_str());
        }
        print_test(test);
    }
    println!();
    println!(
        "提示: 该报告不会自动执行任何测试 — 把上面的列表传给你的测试运行器（pytest/dart test/go test 等）即可。"
    );
}

fn print_test(test: &SelectedTest) {
    let range = test
        .line_range
        .map(|(s, e)| format!(":{s}-{e}"))
        .unwrap_or_default();
    println!(
        "  - {label}  ({kind})",
        label = test.label,
        kind = test.kind
    );
    println!("      id:   {}", test.id);
    println!("      路径: {}{range}", test.path);
    println!("      原因:");
    for r in &test.reasons {
        println!("        - {r}");
    }
}

fn confidence_label(c: &str) -> &'static str {
    match c {
        "high" => "high — 强相关，必须跑",
        "medium" => "medium — 间接相关，建议跑",
        _ => "low — 弱相关，仅在 --include-deps 模式下出现",
    }
}

#[cfg(test)]
mod tests {
    use groundgraph_engine::test_selection::{
        SelectedTest, TestSelection, TestSelectionStats, TEST_SELECTION_SCHEMA_VERSION,
    };

    #[test]
    fn json_output_includes_schema_and_confidence() {
        let report = TestSelection {
            schema_version: TEST_SELECTION_SCHEMA_VERSION,
            base: "main".into(),
            head: "HEAD".into(),
            stats: TestSelectionStats {
                changed_files: 1,
                changed_symbols: 1,
                tests_selected: 1,
                empty: false,
            },
            tests: vec![SelectedTest {
                id: "test::backend/tests/test_foo.py::test_login".into(),
                kind: "test_case".into(),
                label: "test_login".into(),
                path: "backend/tests/test_foo.py".into(),
                line_range: Some((10, 25)),
                reasons: vec!["test_file_directly_changed".into()],
                confidence: "high".into(),
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"confidence\":\"high\""));
        assert!(json.contains("\"test_file_directly_changed\""));
    }
}
