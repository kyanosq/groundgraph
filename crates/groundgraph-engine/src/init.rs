//! `groundgraph init` behaviour.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{
    config_path_for_repo, resolve_links_path, resolve_storage_path, EngineConfig,
    LanguageSelection, CONFIG_SCHEMA_VERSION, DEFAULT_STORAGE_DIR,
};
use crate::requirements_md_indexer::DEFAULT_REQUIREMENTS_DIR;

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub repo_root: PathBuf,
}

impl InitOptions {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }
}

/// The on-disk artefacts produced by `groundgraph init`.
#[derive(Debug, Clone)]
pub struct InitOutcome {
    pub config_path: PathBuf,
    pub config_already_existed: bool,
    pub links_path: PathBuf,
    pub links_already_existed: bool,
    pub graph_db_path: PathBuf,
    pub graph_db_already_existed: bool,
    /// P23.9 — `.groundgraph/requirements/` (Markdown requirements).
    pub requirements_dir: PathBuf,
    pub requirements_already_existed: bool,
}

/// Chinese-first starter file written into `.groundgraph/requirements/` on a
/// fresh init so users have a copyable template.
/// Scaffolded `README.md` explaining the Markdown requirements format. It is
/// intentionally *not* indexed as a requirement (the indexer skips `README.md`)
/// so a fresh `init` leaves the graph empty — mirroring the empty `links.yaml`
/// manifest. The example lives inside a code fence so it never parses as a live
/// requirement.
const SAMPLE_REQUIREMENT_MD: &str = "# GroundGraph 需求映射（Markdown）\n\n\
在本目录新增 `*.md` 文件，声明“需求 → 文档 / 实现 / 测试”的映射。\n\
    索引时 GroundGraph 会读取它们并写入图谱；本目录非侵入，只属于 `.groundgraph/`，不改业务代码。\n\
（`README.md` 仅为说明文档，不会被当作需求解析。）\n\n\
## 文件格式\n\n\
- 每个需求以一级标题开头：`# <编号> <标题>`，`<编号>` 为首个空白前的标记（如 `REQ-001`）。\n\
- 三个可选小节，中英皆可：`## 文档` / `## 实现` / `## 测试`。\n\
- 每个小节是 `路径#片段` 列表；片段可为符号名、类名、`类型.方法` 或文档小节标题；省略 `#片段` 表示整文件。\n\n\
## 示例\n\n\
复制下面代码块内容到新文件（如 `0001-示例.md`）即可：\n\n\
```markdown\n\
# REQ-001 示例需求标题\n\n\
用一句话描述需求意图（可多行），将作为 Requirement 节点的描述。\n\n\
## 文档\n\
- docs/example.md#示例小节\n\n\
## 实现\n\
- lib/example.dart#ExampleClass\n\n\
## 测试\n\
- test/example_test.dart#示例用例\n\
```\n";

/// Initialise a GroundGraph workspace at `options.repo_root`.
///
/// Behaviour:
/// - If `.groundgraph.yaml` is missing, write a default config. Existing config
///   files are left untouched (idempotent re-init).
/// - Ensure `.groundgraph/` exists and open the SQLite database. The database
///   file is created if it is missing.
/// - Ensure the external links manifest exists. This is the only place where
///   users declare requirement-to-code/test relationships.
pub fn init_repository(options: InitOptions) -> Result<InitOutcome> {
    let repo_root = options.repo_root;
    let config_path = config_path_for_repo(&repo_root);
    let config_already_existed = config_path.exists();

    let config = if config_already_existed {
        load_existing_config(&config_path)?
    } else {
        let cfg = default_config_for(&repo_root);
        let yaml = serde_yml::to_string(&cfg).context("serialising default config to YAML")?;
        std::fs::write(&config_path, yaml)
            .with_context(|| format!("writing default config to {}", config_path.display()))?;
        cfg
    };

    let storage_dir = resolve_storage_path(&repo_root, &config)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo_root.join(DEFAULT_STORAGE_DIR));
    std::fs::create_dir_all(&storage_dir)
        .with_context(|| format!("creating storage directory {}", storage_dir.display()))?;

    let links_path = resolve_links_path(&repo_root, &config);
    let links_already_existed = links_path.exists();
    if !links_already_existed {
        if let Some(parent) = links_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating links directory {}", parent.display()))?;
        }
        std::fs::write(&links_path, "requirements: {}\n")
            .with_context(|| format!("writing links manifest {}", links_path.display()))?;
    }

    // P23.9 — scaffold the Markdown requirements directory with a template.
    let requirements_dir = repo_root.join(DEFAULT_REQUIREMENTS_DIR);
    let requirements_already_existed = requirements_dir.exists();
    if !requirements_already_existed {
        std::fs::create_dir_all(&requirements_dir).with_context(|| {
            format!(
                "creating requirements directory {}",
                requirements_dir.display()
            )
        })?;
        let sample = requirements_dir.join("README.md");
        std::fs::write(&sample, SAMPLE_REQUIREMENT_MD)
            .with_context(|| format!("writing requirements README {}", sample.display()))?;
    }

    let graph_db_path = resolve_storage_path(&repo_root, &config);
    let graph_db_already_existed = graph_db_path.exists();

    let mut store = groundgraph_store::Store::open(&graph_db_path)
        .with_context(|| format!("opening SQLite database at {}", graph_db_path.display()))?;
    store
        .migrate()
        .with_context(|| format!("running migrations on {}", graph_db_path.display()))?;
    drop(store);

    Ok(InitOutcome {
        config_path,
        config_already_existed,
        links_path,
        links_already_existed,
        graph_db_path,
        graph_db_already_existed,
        requirements_dir,
        requirements_already_existed,
    })
}

/// Build the config written on a fresh `init`.
///
/// Write unified `languages:` entries pointing at detected source roots and
/// disable LSP enrichment so a freshly-initialised workspace gets a working
/// graph from the always-available, dependency-free tree-sitter backend.
fn default_config_for(repo_root: &Path) -> EngineConfig {
    let selections = detect_language_selections(repo_root);
    let mut cfg = EngineConfig {
        languages: selections,
        ..EngineConfig::default()
    };
    // Zero external dependencies out of the box; the per-language LSP
    // adapters can be opted back in by flipping this flag.
    cfg.enrichment.lsp = false;
    // `languages` is authoritative: `normalized()` repopulates the Dart
    // `code` section from a `dart` selection when present, else clears it.
    cfg.code.paths = Vec::new();
    // #72 — stamp every freshly-written config with the current schema version
    // so a future build can detect (and warn about) version skew.
    cfg.schema_version = Some(CONFIG_SCHEMA_VERSION);
    cfg
}

/// Directories that never hold first-party sources worth indexing — version
/// control, dependency caches, and build outputs across ecosystems.
const DETECT_SKIP_DIRS: &[&str] = &[
    ".git",
    ".build",
    ".swiftpm",
    "DerivedData",
    "Pods",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".dart_tool",
    DEFAULT_STORAGE_DIR,
    "generated",
    ".next",
    "venv",
    ".venv",
    "__pycache__",
    ".idea",
    ".vscode",
    ".gradle",
    // Documentation and vendored-reference trees hold *citation* code —
    // third-party source copies, spike notes, framework excerpts. Indexing
    // them as first-party roots floods dead-code / similarity / search with
    // someone else's symbols.
    "docs",
    "doc",
    "vendor",
    "third_party",
    "thirdparty",
    "external",
    "references",
    "release-scans",
    // C-ecosystem convention (Redis, many Makefile projects): bundled
    // third-party sources live under `deps/`.
    "deps",
    // Go toolchain convention: `testdata/` is invisible to the compiler —
    // its .go files are fixtures (often intentionally broken or generated)
    // and must never elect a language or become an indexing root.
    "testdata",
];

/// Map a lower-case file extension to its canonical GroundGraph language id.
fn ext_language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "swift" => "swift",
        "dart" => "dart",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" => "typescript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "cs" => "csharp",
        "rb" | "rake" => "ruby",
        "php" => "php",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        _ => return None,
    })
}

/// C/C++ *header* extensions. A header declares but never defines: a directory
/// of headers alone is not a translation unit and must not, by itself, elect a
/// C/C++ project. This matters most for `.h`, which is ambiguous — in an
/// Objective-C / Swift iOS app `.h` files are Obj-C headers (often a bridging
/// header), so counting them as C would elect a phantom `c` project rooted at
/// the Swift app and feed Obj-C headers to the C parser. A language is elected
/// only by a real translation unit (`.c` / `.cc` / `.cpp` / `.cxx`); once
/// elected, its source dirs are indexed normally, headers included.
///
/// Trade-off: a header-only C++ library (`.hpp` with no `.cpp`) no longer
/// self-elects. Such repos almost always ship example/test translation units;
/// if not, add the language to `languages:` manually.
fn ext_is_header(ext: &str) -> bool {
    matches!(ext, "h" | "hpp" | "hh" | "hxx")
}

/// Flutter / React-Native / desktop *platform-embedding* directories. They
/// hold generated glue (`GeneratedPluginRegistrant.java`), native scaffolding
/// (`AppDelegate.swift`, `MainActivity.kt`) and build manifests
/// (`build.gradle`) — never first-party application logic. Skipping them
/// *during language detection* stops a Flutter app's Android/iOS scaffolding
/// from electing a phantom `java`/`swift` project. (They are still indexed
/// normally once their real language is selected.)
const DETECT_EMBED_DIRS: &[&str] = &["android", "ios", "macos", "windows", "linux"];

/// Conventional build-output excludes for a detected language.
fn language_build_excludes(lang: &str) -> Vec<String> {
    match lang {
        "swift" => vec!["**/.build/**".into()],
        "rust" => vec!["**/target/**".into()],
        "typescript" => vec!["**/node_modules/**".into(), "**/dist/**".into()],
        "python" => vec!["**/.venv/**".into(), "**/__pycache__/**".into()],
        "go" => vec!["**/vendor/**".into()],
        "java" => vec!["**/target/**".into(), "**/build/**".into()],
        "csharp" => vec!["**/bin/**".into(), "**/obj/**".into()],
        "ruby" => vec!["**/vendor/**".into(), "**/tmp/**".into()],
        "php" => vec!["**/vendor/**".into(), "**/storage/**".into()],
        "kotlin" => vec!["**/build/**".into(), "**/.gradle/**".into()],
        _ => Vec::new(),
    }
}

/// Detect *every* first-party language present in the repo and the top-level
/// source roots each lives under.
///
/// A polyglot monorepo (Go backend + Flutter app + Swift app + TS admin web,
/// …) must enable all of them, so this returns one [`LanguageSelection`] per
/// language that has at least one real source file — including Dart, which the
/// caller routes onto the legacy `code` section via `normalized()`. Returns an
/// empty vec when nothing recognisable is found, so the caller falls back to
/// the historical Dart default.
///
/// Selection is by *source-file presence only* — build manifests are
/// deliberately not scored, because nested build files (Flutter's
/// `android/build.gradle`, an `ios/Podfile`, …) would otherwise elect phantom
/// languages. Platform-embedding dirs are skipped entirely. The walk is
/// bounded (depth + file cap) and skips VCS / dependency / build directories.
/// Manifest files that declare a first-party language project. A language
/// backed by a manifest is elected even with a single source file; without
/// one it needs more than a trace presence (stray gdb scripts, codegen
/// helpers) to justify an indexing pass.
const LANGUAGE_MANIFESTS: &[(&str, &str)] = &[
    ("pubspec.yaml", "dart"),
    ("go.mod", "go"),
    ("package.swift", "swift"),
    ("package.json", "typescript"),
    ("cargo.toml", "rust"),
    ("composer.json", "php"),
    ("gemfile", "ruby"),
    ("pom.xml", "java"),
    ("build.gradle", "java"),
    ("build.gradle.kts", "kotlin"),
    ("pyproject.toml", "python"),
    ("requirements.txt", "python"),
    ("setup.py", "python"),
];

pub(crate) fn detect_language_selections(repo_root: &Path) -> Vec<LanguageSelection> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut manifest_langs: BTreeSet<&'static str> = BTreeSet::new();
    let mut topdirs: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    // Header-rich dirs (fmt's `include/`): headers never *elect* a language,
    // but once a real translation unit elects C/C++, the header dirs must
    // join its roots or a header-only library body is silently skipped.
    // `.h` is ambiguous between C and C++, so it accrues to both candidates;
    // only elected languages consume the set.
    let mut header_topdirs: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();

    let mut stack: Vec<(PathBuf, usize)> = vec![(repo_root.to_path_buf(), 0)];
    let mut visited_files = 0usize;
    'walk: while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if depth >= 12
                    || name.starts_with('.')
                    || DETECT_SKIP_DIRS.contains(&name.as_str())
                    || DETECT_EMBED_DIRS.contains(&name.as_str())
                {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if ft.is_file() {
                visited_files += 1;
                if visited_files > 200_000 {
                    break 'walk;
                }
                let path = entry.path();
                let lower_name = name.to_ascii_lowercase();
                if let Some((_, lang)) = LANGUAGE_MANIFESTS
                    .iter()
                    .find(|(manifest, _)| *manifest == lower_name)
                {
                    manifest_langs.insert(lang);
                }
                if lower_name.ends_with(".csproj") {
                    manifest_langs.insert("csharp");
                }
                let Some(ext) = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase)
                else {
                    continue;
                };
                let Some(lang) = ext_language(&ext) else {
                    continue;
                };
                let topdir_of = |path: &Path| -> Option<String> {
                    let rel = path.strip_prefix(repo_root).ok()?;
                    let mut comps = rel.components();
                    let first = comps.next();
                    let has_more = comps.next().is_some();
                    Some(match (first, has_more) {
                        (Some(c), true) => c.as_os_str().to_string_lossy().into_owned(),
                        _ => ".".to_string(), // file directly at the repo root
                    })
                };
                // Headers declare but do not define: they must not elect a
                // language on their own (see `ext_is_header`), but their dirs
                // are recorded so an *elected* C/C++ still indexes them.
                if ext_is_header(&ext) {
                    if let Some(topdir) = topdir_of(&path) {
                        if ext == "h" {
                            // Ambiguous: claimable by either C or C++.
                            header_topdirs
                                .entry("c")
                                .or_default()
                                .insert(topdir.clone());
                            header_topdirs.entry("cpp").or_default().insert(topdir);
                        } else {
                            header_topdirs.entry("cpp").or_default().insert(topdir);
                        }
                    }
                    continue;
                }
                *counts.entry(lang).or_default() += 1;
                if let Some(topdir) = topdir_of(&path) {
                    topdirs.entry(lang).or_default().insert(topdir);
                }
            }
        }
    }

    // One selection per language with ≥1 real source file, deterministically
    // ordered by language id. Languages without a manifest need more than a
    // trace presence (≥3 files or ≥25% of the repo's sources).
    let total: usize = counts.values().sum();
    counts
        .into_iter()
        .filter(|(lang, c)| *c > 0 && (manifest_langs.contains(lang) || *c >= 3 || *c * 4 >= total))
        .map(|(lang, _)| {
            let mut roots = topdirs.get(lang).cloned().unwrap_or_default();
            // Elected C/C++ also owns its header-rich dirs (fmt's
            // header-only `include/`). Unelected languages never get here,
            // so Obj-C headers still cannot create a phantom C project.
            if let Some(hdrs) = header_topdirs.get(lang) {
                roots.extend(hdrs.iter().cloned());
            }
            let mut paths: Vec<String> = roots.into_iter().collect();
            paths.sort();
            LanguageSelection {
                id: lang.to_string(),
                paths,
                exclude: language_build_excludes(lang),
                lsp_command: None,
            }
        })
        .collect()
}

fn load_existing_config(path: &Path) -> Result<EngineConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading existing config {}", path.display()))?;
    let config = serde_yml::from_str::<EngineConfig>(&contents)
        .with_context(|| format!("parsing existing config {}", path.display()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn ids(sels: &[LanguageSelection]) -> Vec<String> {
        let mut v: Vec<String> = sels.iter().map(|s| s.id.clone()).collect();
        v.sort();
        v
    }

    /// A polyglot monorepo (FitHub-shaped) must enable *every* first-party
    /// language, not just the single highest-scoring one.
    #[test]
    fn detects_all_first_party_languages_in_monorepo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Go modules.
        write(root, "backend/go.mod", "module x\n");
        write(
            root,
            "backend/internal/api/server.go",
            "package api\nfunc Serve(){}\n",
        );
        write(root, "piclient/go.mod", "module y\n");
        write(
            root,
            "piclient/internal/m/run.go",
            "package m\nfunc Run(){}\n",
        );
        // Dart app.
        write(root, "apps/app/pubspec.yaml", "name: app\n");
        write(root, "apps/app/lib/main.dart", "void main() {}\n");
        // Swift app (SwiftPM-style sources).
        write(
            root,
            "apps/Studio/Sources/Core/model.swift",
            "struct M {}\n",
        );
        write(root, "apps/Studio/Sources/Core/view.swift", "struct V {}\n");
        // TypeScript admin web.
        write(root, "backend/web/package.json", "{}\n");
        write(root, "backend/web/src/client.ts", "export const x = 1;\n");

        let sels = detect_language_selections(root);
        assert_eq!(
            ids(&sels),
            vec!["dart", "go", "swift", "typescript"],
            "every first-party language must be selected"
        );
    }

    /// Regression (rust-analyzer): a single gdb pretty-printer `.py` among
    /// ~1500 `.rs` files made `init` enable a python pass over `lib/` —
    /// which is a Rust directory. A language with no manifest and only a
    /// trace presence is build tooling, not a first-party language.
    /// Manifest-backed languages (pubspec, package.json…) stay elected even
    /// with one file.
    #[test]
    fn stray_single_script_does_not_elect_a_language() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[workspace]\n");
        for i in 0..6 {
            write(root, &format!("crates/a/src/f{i}.rs"), "pub fn x() {}\n");
        }
        write(root, "lib/smol_str/src/gdb_printer.py", "print('x')\n");
        let sels = detect_language_selections(root);
        assert_eq!(
            ids(&sels),
            vec!["rust"],
            "a stray script must not elect python"
        );
    }

    /// Flutter / React-Native platform-embedding dirs (`android`, `ios`,
    /// `macos`) hold generated/scaffolding sources — they must NOT elect a
    /// phantom language (the original bug: Flutter's `android/build.gradle` +
    /// `GeneratedPluginRegistrant.java` made `java` win and suppressed Go/Dart).
    #[test]
    fn ignores_flutter_platform_embed_scaffolding() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "apps/app/pubspec.yaml", "name: app\n");
        write(root, "apps/app/lib/main.dart", "void main() {}\n");
        // Flutter Android embedding — generated Java + gradle manifests.
        write(root, "apps/app/android/build.gradle", "// gradle\n");
        write(
            root,
            "apps/app/android/app/src/main/java/io/flutter/plugins/GeneratedPluginRegistrant.java",
            "package io.flutter.plugins; class GeneratedPluginRegistrant {}\n",
        );
        // Flutter iOS/macOS embedding — scaffolding Swift.
        write(
            root,
            "apps/app/ios/Runner/AppDelegate.swift",
            "import UIKit\n",
        );
        write(
            root,
            "apps/app/macos/Runner/AppDelegate.swift",
            "import Cocoa\n",
        );

        let sels = detect_language_selections(root);
        assert_eq!(
            ids(&sels),
            vec!["dart"],
            "only the real Dart app, no phantom java/swift"
        );
    }

    /// A pure-Dart repo still resolves to a single Dart selection scoped to its
    /// real source dirs (so the legacy `code` section keeps working).
    #[test]
    fn pure_dart_repo_selects_dart_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "pubspec.yaml", "name: app\n");
        write(root, "lib/main.dart", "void main() {}\n");
        write(root, "test/main_test.dart", "void main() {}\n");

        let sels = detect_language_selections(root);
        assert_eq!(ids(&sels), vec!["dart"]);
        let dart = sels.iter().find(|s| s.id == "dart").unwrap();
        let mut paths = dart.paths.clone();
        paths.sort();
        assert_eq!(paths, vec!["lib", "test"]);
    }

    /// Code that lives under documentation / vendored-reference directories
    /// is citation material, not first-party source: a `docs/references/`
    /// copy of a third-party framework must not elect `docs` as a Python
    /// code root (dogfooding MetaQuant indexed a vendored rqalpha tree and
    /// reported 880 dead symbols from it).
    #[test]
    fn docs_and_vendored_dirs_do_not_elect_code_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/app.py", "def main():\n    pass\n");
        write(
            root,
            "docs/references/rqalpha-source/rqalpha/api.py",
            "def order_shares():\n    pass\n",
        );
        write(root, "vendor/lib/util.py", "def helper():\n    pass\n");
        write(root, "third_party/sdk/sdk.py", "def sdk():\n    pass\n");
        write(
            root,
            "references/spike/proto.py",
            "def proto():\n    pass\n",
        );
        // Redis-style C project: bundled third-party sources under `deps/`.
        write(
            root,
            "deps/jemalloc/src/arena.c",
            "int arena(void) { return 0; }\n",
        );
        write(root, "deps/lua/src/lvm.c", "int lvm(void) { return 0; }\n");

        let sels = detect_language_selections(root);
        assert_eq!(ids(&sels), vec!["python"]);
        let py = sels.iter().find(|s| s.id == "python").unwrap();
        assert_eq!(
            py.paths,
            vec!["src"],
            "docs/vendor/third_party/references/deps must not become code roots"
        );
    }

    /// Release scan scratch trees are external target snapshots captured for
    /// reports. They can contain many languages, but they are not first-party
    /// source for the GroundGraph repo itself and must not create dogfood
    /// "language present but unindexed" warnings.
    #[test]
    fn release_scan_snapshots_do_not_elect_languages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[workspace]\n");
        write(root, "crates/app/src/lib.rs", "pub fn run() {}\n");
        for i in 0..3 {
            write(
                root,
                &format!("release-scans/_scratch/ext/cpp/file{i}.cpp"),
                "int x(void) { return 0; }\n",
            );
            write(
                root,
                &format!("release-scans/_scratch/ext/csharp/File{i}.cs"),
                "class X {}\n",
            );
            write(
                root,
                &format!("release-scans/_scratch/ext/ruby/file{i}.rb"),
                "def x\nend\n",
            );
        }

        let sels = detect_language_selections(root);
        assert_eq!(
            ids(&sels),
            vec!["rust"],
            "release-scans snapshots must not elect external languages"
        );
    }

    /// fmt-shaped C++ library: the entire public surface lives in
    /// header-only `include/fmt/*.h`, with only a couple of `.cc`
    /// translation units under `src/`. Headers must not *elect* a language
    /// (Obj-C protection below), but once real TUs elect it, the
    /// header-rich dirs must join the roots — otherwise the library body
    /// is silently skipped (fmt: 0 of ~20k header lines indexed).
    #[test]
    fn header_dirs_join_roots_once_a_translation_unit_elects_the_language() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/format.cc", "int x;\n");
        write(root, "include/fmt/format.h", "class Formatter {};\n");
        write(root, "include/fmt/base.h", "class Base {};\n");

        let sels = detect_language_selections(root);
        let cpp = sels.iter().find(|s| s.id == "cpp").expect("cpp elected");
        assert!(
            cpp.paths.contains(&"include".to_string()),
            "header-only include/ must join cpp roots: {:?}",
            cpp.paths
        );
        assert!(cpp.paths.contains(&"src".to_string()));
        // Headers alone still must not elect C.
        assert!(!sels.iter().any(|s| s.id == "c"), "no phantom c project");
    }

    /// An empty / unrecognised tree yields no selections (caller falls back to
    /// the legacy Dart default).
    #[test]
    fn empty_tree_yields_no_selection() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# hi\n");
        assert!(detect_language_selections(dir.path()).is_empty());
    }

    /// An iOS / Swift app's Objective-C headers (`.h`, often a bridging header)
    /// must NOT elect a phantom `c` project. Headers declare but never define;
    /// a directory of `.h` alone is not a translation unit. The original bug:
    /// a 1000-file Swift app with 11 Obj-C `.h` files elected `c` rooted at the
    /// whole Swift source dir, and the C indexer then tried to parse Obj-C
    /// headers as C.
    #[test]
    fn objc_headers_alone_do_not_elect_phantom_c() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Yolan/Sources/view.swift", "struct V {}\n");
        write(root, "Yolan/Sources/model.swift", "struct M {}\n");
        // Obj-C bridging header + Obj-C implementation (`.m` is unsupported and
        // ignored). No `.c` translation unit anywhere.
        write(
            root,
            "Yolan/OCFiles/Yolan-Bridging-Header.h",
            "#import <Foundation/Foundation.h>\n",
        );
        write(
            root,
            "Yolan/OCFiles/Helper.m",
            "@implementation Helper\n@end\n",
        );

        let sels = detect_language_selections(root);
        assert_eq!(
            ids(&sels),
            vec!["swift"],
            "Obj-C headers must not elect a phantom c project"
        );
    }

    /// The header gate must not over-correct: a real C or C++ *translation
    /// unit* (`.c` / `.cpp`) still elects its language, and its headers are
    /// part of that project's source dirs.
    #[test]
    fn c_and_cpp_translation_units_still_elect() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "csrc/util.c", "int answer(void){return 42;}\n");
        write(root, "csrc/util.h", "int answer(void);\n");
        write(root, "cpp/calc.cpp", "int twice(int x){return x*2;}\n");
        write(root, "cpp/calc.hpp", "int twice(int);\n");

        let sels = detect_language_selections(root);
        assert_eq!(
            ids(&sels),
            vec!["c", "cpp"],
            "real .c / .cpp translation units still elect their language"
        );
    }
}
