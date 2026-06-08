//! Shared path-classification heuristics.
//!
//! "Is this a test file?" and "is this machine-generated codegen?" are asked
//! by several analyses (dead-code ranking, business candidates, porting
//! coverage). The logic is identical everywhere, so it lives here once.

/// Heuristic: does this path live in test / spec scaffolding?
pub fn is_test_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
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
    in_test_dir
        || p.ends_with("_test.dart")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_spec.")
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
        assert!(!is_test_path("lib/models/shift.dart"));
        assert!(!is_test_path("App/Views/Home/HomeView.swift"));
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
