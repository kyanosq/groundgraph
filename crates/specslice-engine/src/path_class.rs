//! Shared path-classification heuristics.
//!
//! "Is this a test file?" and "is this machine-generated codegen?" are asked
//! by several analyses (dead-code ranking, business candidates, porting
//! coverage). The logic is identical everywhere, so it lives here once.

/// Heuristic: does this path live in test / spec scaffolding?
pub fn is_test_path(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let p = norm.to_ascii_lowercase();
    let in_test_dir = p.split('/').any(|seg| {
        matches!(
            seg,
            "test"
                | "tests"
                | "testing"
                | "integration_test"
                | "test_driver"
                | "__tests__"
                | "spec"
                | "specs"
        )
    });
    // pytest/Go conventions on the file name itself: `test_*.py` (prefix) and
    // `*_test.*` (suffix). The prefix is a basename check so a business word
    // like `contest_results.py` (substring `test_`) never matches.
    let base = p.rsplit('/').next().unwrap_or(&p);
    if in_test_dir
        || base.starts_with("test_")
        || p.ends_with("_test.dart")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_spec.")
    {
        return true;
    }
    // Xcode / SPM target dirs (`CleanerTests/`, `AppUITests/`) and
    // XCTest / JUnit file names (`FooTests.swift`, `FooTest.java`). The
    // uppercase `Test` requires a CamelCase word boundary, so business
    // words that merely end in "test(s)" (`contests/`, `latest.rs`) and
    // lowercase stems never match.
    let camel_test_suffix = |s: &str| s.ends_with("Tests") || s.ends_with("Test");
    if norm.split('/').rev().skip(1).any(&camel_test_suffix) {
        return true;
    }
    let stem = norm
        .rsplit('/')
        .next()
        .map(|base| base.split('.').next().unwrap_or(base))
        .unwrap_or(&norm);
    camel_test_suffix(stem)
}

/// Heuristic: auxiliary (non-production) code — developer tools, benchmarks,
/// examples, demo apps. Real code, but not the product: when ranking search
/// hits or naming business modules, production sources should outrank these.
/// Distinct from [`is_test_path`] so callers can treat tests separately.
pub fn is_auxiliary_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    p.split('/').any(|seg| {
        matches!(
            seg,
            "tools"
                | "tool"
                | "scripts"
                | "script"
                | "bench"
                | "benches"
                | "benchmark"
                | "benchmarks"
                | "examples"
                | "example"
                | "demo"
                | "demos"
                | "samples"
                | "sample"
                | "fixtures"
                | "playground"
        )
    })
}

/// Heuristic: is this a generated / codegen file? Such symbols (freezed
/// `copyWith` impls, json_serializable `.g.dart`, protobuf, mockito mocks,
/// Flutter l10n `app_localizations*`) are machine-written plumbing.
pub fn is_generated_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    const GENERATED_SUFFIXES: &[&str] = &[
        ".freezed.dart",
        ".g.dart",
        ".gr.dart",
        ".config.dart",
        ".mocks.dart",
        ".pb.dart",
        ".pbenum.dart",
        ".pbjson.dart",
        ".pbserver.dart",
        ".gen.dart",
    ];
    if GENERATED_SUFFIXES.iter().any(|suf| p.ends_with(suf))
        || p.ends_with(".generated.ts")
        || p.ends_with(".g.ts")
    {
        return true;
    }
    // Flutter localisation delegates: lib/l10n/app_localizations*.dart and the
    // `/generated/` convention used by many codegen tools.
    let base = p.rsplit('/').next().unwrap_or(&p);
    base.starts_with("app_localizations") || p.contains("/generated/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_test_paths() {
        assert!(is_test_path("test/foo_test.dart"));
        assert!(is_test_path(
            "ShiftCore/Tests/ShiftCoreTests/AlarmTests.swift"
        ));
        assert!(is_test_path("src/__tests__/a.ts"));
        assert!(is_test_path("lib/a.spec.ts"));
        // Go suffix and pytest prefix conventions outside any `test/` dir.
        assert!(is_test_path("pkg/handler_test.go"));
        assert!(is_test_path("app/services/test_auth.py"));
        assert!(!is_test_path("lib/models/shift.dart"));
        // `test_`/`_test` prefix/suffix must be a real word boundary, not a
        // substring of a business name.
        assert!(!is_test_path("src/contest_results.py"));
        assert!(!is_test_path("lib/fastest.go"));
        assert!(!is_test_path("App/Views/Home/HomeView.swift"));
        // Xcode / SPM / JUnit conventions: `<Target>Tests/` target dirs and
        // `FooTests.swift` / `FooTest.java` files outside any `test/` dir.
        assert!(is_test_path(
            "CleanerTests/SimilarityAnalysisServiceTests.swift"
        ));
        assert!(is_test_path("CleanerUITests/CleanerUITests.swift"));
        assert!(is_test_path("src/main/java/ManifestTest.java"));
        // CamelCase boundary required — these are business files.
        assert!(!is_test_path("docs/contests/rules.md"));
        assert!(!is_test_path("lib/latest.rs"));
    }

    #[test]
    fn detects_auxiliary_paths() {
        assert!(is_auxiliary_path("tools/array-bench.py"));
        assert!(is_auxiliary_path("scripts/create-cluster/clean.sh"));
        assert!(is_auxiliary_path("examples/http/main.go"));
        assert!(is_auxiliary_path("benchmarks/run.py"));
        assert!(!is_auxiliary_path("src/networking.c"));
        assert!(!is_auxiliary_path("lib/services/auth.dart"));
        // `test` dirs are NOT auxiliary — they have their own classifier.
        assert!(!is_auxiliary_path("test/foo_test.dart"));
    }

    #[test]
    fn detects_generated_paths() {
        assert!(is_generated_path("lib/models/alarm.freezed.dart"));
        assert!(is_generated_path("lib/premium/product.g.dart"));
        assert!(is_generated_path("lib/l10n/app_localizations_en.dart"));
        assert!(is_generated_path("lib/generated/assets.dart"));
        assert!(!is_generated_path("lib/models/alarm.dart"));
        assert!(!is_generated_path(
            "ShiftCore/Sources/ShiftCore/Models/Alarm.swift"
        ));
    }
}
