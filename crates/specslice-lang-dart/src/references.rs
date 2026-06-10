//! Body-level reference scanner for the Dart lightweight adapter.
//!
//! The MVP parser only records declarations. To make `specslice graph
//! --focus <method>` show a real code-fact chain we also need lightweight
//! "this method body mentions that class / calls that method" edges. Doing a
//! full Dart resolution is out of scope (PRD §2.2 / Phase 3 with the analyzer
//! sidecar), so we approximate with a tiny pass on the textual body:
//!
//! 1. Iterate every parsed file with its source.
//! 2. For each method / function / constructor symbol, look at the lines
//!    between its declaration and closing brace.
//! 3. Strip comments and string literals, then find identifiers.
//! 4. For each identifier, decide whether it is a member call
//!    (`.name(`), a free call (`name(`) or a class-style reference
//!    (`Name.something` / `Name foo` / `Name,` …).
//! 5. Look the identifier up against the symbols indexed across the batch
//!    and emit one [`ReferenceEdge`] per matched target, skipping
//!    self-references.
//!
//! The heuristic deliberately under-emits rather than over-emits: it only
//! produces edges when the textual shape matches one of the patterns above
//! and the name resolves to a symbol the adapter already parsed. False
//! positives are still possible for genuinely homonymous symbols, but the
//! engine treats every edge as a Fact-layer hint, not as a confirmed link.

use std::collections::{BTreeMap, HashMap};

use specslice_core::language_batch::{ReferenceEdge, SymbolArtifact, SymbolRange};
use specslice_core::{ArtifactId, EdgeKind, NodeKind};

/// String constant the Dart lightweight adapter writes into
/// [`ReferenceEdge::resolver`]. The analyzer sidecar will use
/// `dart_analyzer`; AI-derived edges will use `ai_candidate`.
pub const RESOLVER_DART_LIGHTWEIGHT: &str = "dart_lightweight";

#[derive(Debug, Clone)]
pub struct FileSource {
    pub path: String,
    pub source: String,
}

/// Maximum number of characters kept from a source line as evidence.
///
/// Long lines (auto-generated tables, embedded base64, etc.) would bloat the
/// `edge_assertions` table and the JSON exports. 200 characters keeps the
/// snippet legible in the UI without truncating typical Dart statements.
const SNIPPET_MAX_LEN: usize = 200;

/// Names that mostly produce noise when treated as calls/references:
/// every Flutter widget overrides them, so a homonym-only scanner would
/// otherwise wire every widget to every other widget. The list is
/// deliberately small — only entries whose value as a code-fact signal is
/// near-zero. Anything language-specific (Future, Stream, Map) stays
/// scannable.
const FRAMEWORK_NOISE_METHODS: &[&str] = &[
    "toString",
    "hashCode",
    "noSuchMethod",
    "runtimeType",
    "copyWith",
    "dispose",
    "initState",
    "build",
    "didChangeDependencies",
    "didUpdateWidget",
    "deactivate",
    "reassemble",
    "createState",
    "createElement",
];

fn is_framework_noise(name: &str) -> bool {
    FRAMEWORK_NOISE_METHODS.contains(&name)
}

#[derive(Default)]
struct SymbolIndex<'a> {
    classes_by_name: HashMap<&'a str, Vec<&'a SymbolArtifact>>,
    callables_by_name: HashMap<&'a str, Vec<&'a SymbolArtifact>>,
}

impl<'a> SymbolIndex<'a> {
    fn build(symbols: &'a [SymbolArtifact]) -> Self {
        let mut idx = SymbolIndex::default();
        for symbol in symbols {
            match symbol.kind {
                NodeKind::DartClass => idx
                    .classes_by_name
                    .entry(symbol.name.as_str())
                    .or_default()
                    .push(symbol),
                NodeKind::DartMethod | NodeKind::DartFunction | NodeKind::DartConstructor => idx
                    .callables_by_name
                    .entry(symbol.name.as_str())
                    .or_default()
                    .push(symbol),
                _ => {}
            }
        }
        idx
    }
}

/// Options for [`compute_references`].
///
/// The adapter ships *all* edges by default so the store stays a complete
/// record of what the scanner saw. Filtering is done at view time in the
/// engine, which keeps a single index usable for both the clean default
/// view and the `--include-noise` debug view.
#[derive(Debug, Clone)]
pub struct ReferenceOptions {
    /// When `false`, framework noise method names (toString, dispose,
    /// initState, …) are dropped at extraction time. Default is `true`
    /// — emit everything and let the engine filter on demand.
    pub include_noise: bool,
}

impl Default for ReferenceOptions {
    fn default() -> Self {
        Self {
            include_noise: true,
        }
    }
}

pub fn compute_references(
    sources: &[FileSource],
    symbols: &[SymbolArtifact],
    ranges: &[SymbolRange],
    field_types: &BTreeMap<ArtifactId, BTreeMap<String, String>>,
) -> Vec<ReferenceEdge> {
    compute_references_with_options(
        sources,
        symbols,
        ranges,
        field_types,
        &ReferenceOptions::default(),
    )
}

pub fn compute_references_with_options(
    sources: &[FileSource],
    symbols: &[SymbolArtifact],
    ranges: &[SymbolRange],
    field_types: &BTreeMap<ArtifactId, BTreeMap<String, String>>,
    opts: &ReferenceOptions,
) -> Vec<ReferenceEdge> {
    if symbols.is_empty() {
        return Vec::new();
    }
    let index = SymbolIndex::build(symbols);
    let lines_by_path = lines_per_file(sources);
    let mut emitted: std::collections::HashSet<(ArtifactId, ArtifactId, EdgeKind)> =
        std::collections::HashSet::new();
    let mut out: Vec<ReferenceEdge> = Vec::new();

    let mut emit = |from: &ArtifactId,
                    to: &ArtifactId,
                    kind: EdgeKind,
                    src_path: &str,
                    line_no: u32,
                    raw_line: &str| {
        if from == to {
            return;
        }
        if !emitted.insert((from.clone(), to.clone(), kind)) {
            return;
        }
        out.push(ReferenceEdge {
            from_symbol_id: from.clone(),
            to_symbol_id: to.clone(),
            kind,
            source_file: src_path.into(),
            line: line_no,
            snippet: shrink_snippet(raw_line),
            resolver: RESOLVER_DART_LIGHTWEIGHT.into(),
        });
    };

    for symbol in symbols {
        if !matches!(
            symbol.kind,
            NodeKind::DartMethod | NodeKind::DartFunction | NodeKind::DartConstructor
        ) {
            continue;
        }
        let enclosing_fields = symbol
            .parent_symbol_id
            .as_ref()
            .and_then(|cid| field_types.get(cid));
        let (start, end) = ranges
            .iter()
            .find(|r| r.symbol_id == symbol.id)
            .map(|r| (r.start_line, r.end_line))
            .unwrap_or((symbol.start_line, symbol.end_line));
        if end <= start {
            continue;
        }
        let Some(lines) = lines_by_path.get(symbol.path.as_str()) else {
            continue;
        };
        let body_start = start as usize; // skip declaration line
        let body_end = (end as usize).min(lines.len());
        if body_start >= body_end {
            continue;
        }

        for (offset, raw_line) in lines[body_start..body_end].iter().enumerate() {
            // 1-based. Saturate when a file is absurdly long (>4 G lines)
            // rather than truncating silently — we'd already have stopped
            // scanning meaningfully by then.
            let line_no = u32::try_from(body_start + offset + 1).unwrap_or(u32::MAX);
            let cleaned = strip_strings_and_comments(raw_line);
            scan_identifiers(&cleaned, |ident, before, after| {
                if !opts.include_noise && is_framework_noise(ident) {
                    return;
                }
                // Field → type resolution: `field.X` where `field` is a
                // class-level field declared with a known class type ⇒
                // emit a `references` edge to that class.
                if !matches!(before, Some('.')) && matches!(after, Some('.')) {
                    if let Some(matches) = enclosing_fields
                        .and_then(|fields| fields.get(ident))
                        .and_then(|type_name| index.classes_by_name.get(type_name.as_str()))
                    {
                        for target in matches {
                            emit(
                                &symbol.id,
                                &target.id,
                                EdgeKind::References,
                                symbol.path.as_str(),
                                line_no,
                                raw_line,
                            );
                        }
                    }
                }

                let kind_hint = classify(before, after);
                let candidates = match kind_hint {
                    Hint::MemberCall | Hint::FreeCall => index.callables_by_name.get(ident),
                    Hint::ClassReference => index.classes_by_name.get(ident),
                    Hint::Skip => None,
                };
                let Some(matches) = candidates else { return };
                let edge_kind = match kind_hint {
                    Hint::MemberCall | Hint::FreeCall => EdgeKind::Calls,
                    Hint::ClassReference => EdgeKind::References,
                    Hint::Skip => return,
                };
                for target in matches {
                    emit(
                        &symbol.id,
                        &target.id,
                        edge_kind,
                        symbol.path.as_str(),
                        line_no,
                        raw_line,
                    );
                }
            });
        }
    }
    out
}

fn shrink_snippet(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= SNIPPET_MAX_LEN {
        return trimmed.to_string();
    }
    // Avoid splitting in the middle of a multi-byte character — count
    // chars, not bytes.
    let mut out = String::with_capacity(SNIPPET_MAX_LEN + 1);
    for c in trimmed.chars().take(SNIPPET_MAX_LEN) {
        out.push(c);
    }
    out.push('…');
    out
}

fn lines_per_file(sources: &[FileSource]) -> HashMap<&str, Vec<&str>> {
    let mut out = HashMap::with_capacity(sources.len());
    for src in sources {
        out.insert(src.path.as_str(), src.source.lines().collect());
    }
    out
}

#[derive(Debug, Clone, Copy)]
enum Hint {
    MemberCall,
    FreeCall,
    ClassReference,
    Skip,
}

fn classify(before: Option<char>, after: Option<char>) -> Hint {
    let preceded_by_dot = matches!(before, Some('.'));
    let followed_by_paren = matches!(after, Some('('));
    let followed_by_dot = matches!(after, Some('.'));

    if followed_by_paren {
        if preceded_by_dot {
            Hint::MemberCall
        } else {
            Hint::FreeCall
        }
    } else if !preceded_by_dot && followed_by_dot {
        Hint::ClassReference
    } else if !preceded_by_dot {
        // Bareword followed by something neutral: only treat as a class
        // reference when the first character is uppercase, which matches
        // Dart's type-name convention. Avoids matching arbitrary locals
        // against class names.
        Hint::Skip
    } else {
        Hint::Skip
    }
}

fn scan_identifiers<F: FnMut(&str, Option<char>, Option<char>)>(line: &str, mut visit: F) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if is_ident_start(c) {
            let start = i;
            while i < bytes.len() && is_ident_continue(bytes[i] as char) {
                i += 1;
            }
            let ident = &line[start..i];
            let before = if start == 0 {
                None
            } else {
                line[..start].chars().last()
            };
            let after = line[i..].chars().next();
            // Skip pure keyword-ish tokens fast: Dart keywords cannot collide
            // with user symbol names that the adapter records, so this is
            // purely an optimisation. We still let the lookup decide.
            visit(ident, before, after);
        } else {
            i += 1;
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// Erase comment payloads and string literal bodies so identifier scanning
/// does not match against documentation or quoted text. The output keeps
/// punctuation in place (so `before` / `after` heuristics stay correct) and
/// only replaces the inner characters with spaces.
fn strip_strings_and_comments(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut in_string = false;
    let mut quote = b'"';
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == quote && (i == 0 || bytes[i - 1] != b'\\') {
                in_string = false;
                out.push(c as char);
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Line comment — stop processing.
            break;
        }
        if c == b'\'' || c == b'"' {
            in_string = true;
            quote = c;
            out.push(c as char);
            i += 1;
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::artifact_id::{dart_class_id, dart_method_id};
    use specslice_core::language_batch::SymbolArtifact;

    fn class(path: &str, name: &str) -> SymbolArtifact {
        SymbolArtifact {
            id: dart_class_id(path, name),
            kind: NodeKind::DartClass,
            path: path.into(),
            name: name.into(),
            qualified_name: name.into(),
            start_line: 1,
            end_line: 1,
            parent_symbol_id: None,
            metadata_json: None,
        }
    }

    fn method(path: &str, class_name: &str, name: &str, start: u32, end: u32) -> SymbolArtifact {
        SymbolArtifact {
            id: dart_method_id(path, class_name, name),
            kind: NodeKind::DartMethod,
            path: path.into(),
            name: name.into(),
            qualified_name: format!("{class_name}.{name}"),
            start_line: start,
            end_line: end,
            parent_symbol_id: Some(dart_class_id(path, class_name)),
            metadata_json: None,
        }
    }

    fn range_for(sym: &SymbolArtifact) -> SymbolRange {
        SymbolRange {
            file_path: sym.path.clone(),
            symbol_id: sym.id.clone(),
            start_line: sym.start_line,
            end_line: sym.end_line,
            symbol_kind: sym.kind,
            qualified_name: sym.qualified_name.clone(),
            parent_symbol_id: sym.parent_symbol_id.clone(),
        }
    }

    #[test]
    fn member_call_emits_calls_edge() {
        let pro = method("lib/pro.dart", "ProNotifier", "applyPurchase", 5, 7);
        let pay = method("lib/pay.dart", "Pay", "go", 3, 5);
        let symbols = vec![pro.clone(), pay.clone()];
        let ranges = vec![range_for(&pro), range_for(&pay)];
        let sources = vec![
            FileSource {
                path: "lib/pay.dart".into(),
                source: "class Pay {\n  ProNotifier n = ProNotifier();\n  void go() {\n    n.applyPurchase('x');\n  }\n}\n".into(),
            },
            FileSource {
                path: "lib/pro.dart".into(),
                source: "class ProNotifier {\n  ProNotifier();\n  void applyPurchase(String id) {\n  }\n}\n".into(),
            },
        ];

        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        assert!(
            edges.iter().any(|e| e.from_symbol_id == pay.id
                && e.to_symbol_id == pro.id
                && e.kind == EdgeKind::Calls),
            "missing calls edge in: {edges:?}"
        );
    }

    #[test]
    fn class_reference_emits_references_edge() {
        let cls = class("lib/iap.dart", "IapProductIds");
        let pay = method("lib/pay.dart", "Pay", "go", 1, 3);
        let symbols = vec![cls.clone(), pay.clone()];
        let ranges = vec![range_for(&pay)];
        let sources = vec![FileSource {
            path: "lib/pay.dart".into(),
            source: "void go() {\n  final ids = IapProductIds.all;\n}\n".into(),
        }];
        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        assert!(
            edges.iter().any(|e| e.from_symbol_id == pay.id
                && e.to_symbol_id == cls.id
                && e.kind == EdgeKind::References),
            "missing references edge: {edges:?}"
        );
    }

    #[test]
    fn field_typed_access_resolves_to_class_reference() {
        // When `pro: ProNotifier` is declared at class scope, scanning
        // `pro.state` inside another method should emit a `references` edge
        // to `ProNotifier`, even though the bareword `pro` itself is not a
        // class name.
        let pro_cls = class("lib/pro.dart", "ProNotifier");
        let editor_cls = class("lib/editor.dart", "EditorScreen");
        let can = method("lib/editor.dart", "EditorScreen", "canUseProFilter", 3, 5);
        let symbols = vec![pro_cls.clone(), editor_cls.clone(), can.clone()];
        let ranges = vec![range_for(&can)];
        let sources = vec![FileSource {
            path: "lib/editor.dart".into(),
            source: "class EditorScreen {\n  ProNotifier pro = ProNotifier();\n  bool canUseProFilter() {\n    return pro.state;\n  }\n}\n".into(),
        }];
        let mut field_types: BTreeMap<ArtifactId, BTreeMap<String, String>> = BTreeMap::new();
        field_types
            .entry(editor_cls.id.clone())
            .or_default()
            .insert("pro".into(), "ProNotifier".into());

        let edges = compute_references(&sources, &symbols, &ranges, &field_types);
        assert!(
            edges.iter().any(|e| e.from_symbol_id == can.id
                && e.to_symbol_id == pro_cls.id
                && e.kind == EdgeKind::References),
            "field-typed access did not resolve: {edges:?}"
        );
    }

    #[test]
    fn identifier_inside_string_is_ignored() {
        let cls = class("lib/iap.dart", "IapProductIds");
        let pay = method("lib/pay.dart", "Pay", "go", 1, 3);
        let symbols = vec![cls.clone(), pay.clone()];
        let ranges = vec![range_for(&pay)];
        let sources = vec![FileSource {
            path: "lib/pay.dart".into(),
            source: "void go() {\n  final s = 'IapProductIds.all';\n}\n".into(),
        }];
        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        assert!(
            edges.is_empty(),
            "must not match identifiers in string literals: {edges:?}"
        );
    }

    #[test]
    fn underscored_dart_identifiers_match() {
        // Pixcraft's real flow uses Dart library-private prefixes, e.g.
        // `PaywallScreen._listenToPurchaseUpdates` calling
        // `ProNotifier.applyPurchase`. The identifier scanner treats `_`
        // as a regular identifier start, so this must still match.
        let pro = method("lib/pro.dart", "ProNotifier", "applyPurchase", 5, 7);
        let pay = method("lib/pay.dart", "Pay", "_listenToPurchaseUpdates", 3, 5);
        let symbols = vec![pro.clone(), pay.clone()];
        let ranges = vec![range_for(&pro), range_for(&pay)];
        let sources = vec![
            FileSource {
                path: "lib/pay.dart".into(),
                source: "class Pay {\n  ProNotifier n = ProNotifier();\n  void _listenToPurchaseUpdates() {\n    n.applyPurchase('x');\n  }\n}\n".into(),
            },
            FileSource {
                path: "lib/pro.dart".into(),
                source: "class ProNotifier {\n  ProNotifier();\n  void applyPurchase(String id) {\n  }\n}\n".into(),
            },
        ];

        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        assert!(
            edges
                .iter()
                .any(|e| e.from_symbol_id == pay.id && e.kind == EdgeKind::Calls),
            "underscored caller body should still emit calls: {edges:?}",
        );
    }

    #[test]
    fn self_reference_is_skipped() {
        let pro = method("lib/pro.dart", "ProNotifier", "applyPurchase", 1, 5);
        let symbols = vec![pro.clone()];
        let ranges = vec![range_for(&pro)];
        let sources = vec![FileSource {
            path: "lib/pro.dart".into(),
            source: "void applyPurchase(String id) {\n  applyPurchase(id);\n}\n".into(),
        }];
        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        assert!(
            edges.iter().all(|e| e.from_symbol_id != e.to_symbol_id),
            "self-loop produced: {edges:?}"
        );
    }

    #[test]
    fn empty_symbol_list_returns_empty_edges() {
        let edges = compute_references(&[], &[], &[], &BTreeMap::new());
        assert!(edges.is_empty());
    }

    #[test]
    fn missing_source_for_symbol_path_is_skipped_safely() {
        // The symbol's `path` does not appear in `sources` — we must not
        // panic, just return no edges for it.
        let pay = method("lib/pay.dart", "Pay", "go", 1, 3);
        let edges = compute_references(
            &[],
            std::slice::from_ref(&pay),
            &[range_for(&pay)],
            &BTreeMap::new(),
        );
        assert!(edges.is_empty());
    }

    #[test]
    fn bodyless_symbol_is_not_scanned() {
        // start_line == end_line means there's no body. The scanner must
        // skip it instead of mis-counting the declaration line as body.
        let pay = method("lib/pay.dart", "Pay", "go", 1, 1);
        let cls = class("lib/iap.dart", "IapProductIds");
        let sources = vec![FileSource {
            path: "lib/pay.dart".into(),
            source: "void go() => IapProductIds.all;\n".into(),
        }];
        let symbols = vec![pay.clone(), cls.clone()];
        let edges = compute_references(&sources, &symbols, &[range_for(&pay)], &BTreeMap::new());
        assert!(
            edges.is_empty(),
            "bodyless symbol must not yield references: {edges:?}"
        );
    }

    #[test]
    fn class_symbol_skips_body_scan_entirely() {
        // SymbolIndex::build hits the `_ => {}` arm for non-callable,
        // non-class node kinds. Pass a test_case symbol to exercise that.
        let cls = class("lib/iap.dart", "IapProductIds");
        let test = SymbolArtifact {
            id: specslice_core::artifact_id::dart_test_id("lib/x_test.dart", "named"),
            kind: NodeKind::TestCase,
            path: "lib/x_test.dart".into(),
            name: "named".into(),
            qualified_name: "named".into(),
            start_line: 1,
            end_line: 5,
            parent_symbol_id: None,
            metadata_json: None,
        };
        let sources = vec![FileSource {
            path: "lib/x_test.dart".into(),
            source: "void main() {\n  test('uses IapProductIds', () {});\n}\n".into(),
        }];
        let edges = compute_references(&sources, &[cls.clone(), test], &[], &BTreeMap::new());
        assert!(
            edges.is_empty(),
            "test_case kinds must not drive body scan: {edges:?}"
        );
    }

    #[test]
    fn duplicate_calls_are_deduplicated() {
        // Body mentions `n.applyPurchase(...)` twice; only one edge should
        // appear in the output (HashSet dedup branch hit).
        let pro = method("lib/pro.dart", "ProNotifier", "applyPurchase", 1, 3);
        let pay = method("lib/pay.dart", "Pay", "go", 1, 5);
        let symbols = vec![pro.clone(), pay.clone()];
        let ranges = vec![range_for(&pro), range_for(&pay)];
        let sources = vec![
            FileSource {
                path: "lib/pay.dart".into(),
                source: "void go() {\n  n.applyPurchase('a');\n  n.applyPurchase('b');\n  n.applyPurchase('c');\n}\n".into(),
            },
            FileSource {
                path: "lib/pro.dart".into(),
                source: "void applyPurchase(String id) {}\n".into(),
            },
        ];
        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        let calls: Vec<_> = edges
            .iter()
            .filter(|e| e.from_symbol_id == pay.id && e.kind == EdgeKind::Calls)
            .collect();
        assert_eq!(calls.len(), 1, "duplicate calls must be deduped: {edges:?}");
    }

    #[test]
    fn field_typed_access_skips_self_reference() {
        // Pathological: the enclosing class has a field whose type is the
        // class itself. `self.method()` must NOT emit a self-loop.
        let pro_cls = class("lib/pro.dart", "ProNotifier");
        let m = method("lib/pro.dart", "ProNotifier", "go", 1, 3);
        let mut fields: BTreeMap<ArtifactId, BTreeMap<String, String>> = BTreeMap::new();
        fields
            .entry(pro_cls.id.clone())
            .or_default()
            .insert("self".into(), "ProNotifier".into());
        let sources = vec![FileSource {
            path: "lib/pro.dart".into(),
            source: "class ProNotifier {\n  ProNotifier self;\n  void go() { self.go(); }\n}\n"
                .into(),
        }];
        // Even though the field-type resolution would normally emit an
        // edge to ProNotifier (the class), `m`'s parent IS ProNotifier so
        // we'd try to emit ProNotifier→ProNotifier. The self-skip should
        // prevent that.
        let edges = compute_references(
            &sources,
            &[pro_cls.clone(), m.clone()],
            &[range_for(&m)],
            &fields,
        );
        assert!(
            edges.iter().all(|e| e.from_symbol_id != e.to_symbol_id),
            "field-typed self-reference must be filtered: {edges:?}"
        );
    }

    #[test]
    fn classify_returns_skip_for_dot_before_and_no_paren_after() {
        // `foo.bar` (bareword after `.`) is Skip — we cannot tell whether
        // bar is a member, property, or method without a paren.
        assert!(matches!(classify(Some('.'), None), Hint::Skip));
        assert!(matches!(classify(Some('.'), Some(';')), Hint::Skip));
        // `foo;` with no surrounding punctuation is also Skip.
        assert!(matches!(classify(None, Some(';')), Hint::Skip));
        // `Foo(` with no leading dot is FreeCall.
        assert!(matches!(classify(None, Some('(')), Hint::FreeCall));
        // `Foo.` with no leading dot is ClassReference.
        assert!(matches!(classify(None, Some('.')), Hint::ClassReference));
    }

    #[test]
    fn strip_strings_and_comments_handles_escapes_and_mixed_quotes() {
        let cleaned = strip_strings_and_comments(r#"x = "a \"b\" c"; // y = "z""#);
        assert!(cleaned.contains("x ="));
        assert!(!cleaned.contains('y'), "comment payload must be erased");
        // Single quotes work too.
        let cleaned2 = strip_strings_and_comments("x = 'pro_monthly'; // ignored");
        assert!(!cleaned2.contains("pro_monthly"));
    }

    #[test]
    fn scan_identifiers_visits_consecutive_tokens() {
        let mut seen: Vec<String> = Vec::new();
        scan_identifiers("foo bar.baz()", |ident, _, _| {
            seen.push(ident.to_string());
        });
        assert_eq!(seen, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn scan_identifiers_skips_non_identifier_leading_chars() {
        let mut seen: Vec<String> = Vec::new();
        scan_identifiers("   $foo + bar", |ident, _, _| {
            seen.push(ident.to_string());
        });
        // `$foo` starts with `$` which is not is_ident_start, so the `$`
        // is skipped and `foo` is captured. `bar` is captured too.
        assert_eq!(seen, vec!["foo", "bar"]);
    }

    // -----------------------------------------------------------------
    // P6.3 evidence + noise coverage.
    // -----------------------------------------------------------------

    #[test]
    fn p63_shrink_snippet_trims_and_truncates_long_lines() {
        // Short → trimmed, no ellipsis.
        assert_eq!(shrink_snippet("   foo  "), "foo");
        // Empty input stays empty.
        assert_eq!(shrink_snippet("   "), "");
        // Long input gets cut at SNIPPET_MAX_LEN with a `…` marker.
        let long = "x".repeat(SNIPPET_MAX_LEN + 50);
        let s = shrink_snippet(&long);
        assert!(s.ends_with('…'), "{s}");
        assert!(s.chars().count() == SNIPPET_MAX_LEN + 1);
    }

    #[test]
    fn p63_shrink_snippet_handles_multibyte_chars() {
        // CJK chars are 3 bytes each in UTF-8 but should be counted as 1
        // char each by the truncator.
        let cjk = "中".repeat(SNIPPET_MAX_LEN + 5);
        let s = shrink_snippet(&cjk);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() == SNIPPET_MAX_LEN + 1);
    }

    #[test]
    fn p63_emits_evidence_fields_on_every_edge() {
        let pro = method("lib/pro.dart", "ProNotifier", "applyPurchase", 5, 7);
        let pay = method("lib/pay.dart", "Pay", "go", 3, 5);
        let symbols = vec![pro.clone(), pay.clone()];
        let ranges = vec![range_for(&pro), range_for(&pay)];
        let sources = vec![
            FileSource {
                path: "lib/pay.dart".into(),
                source: "class Pay {\n  ProNotifier n = ProNotifier();\n  void go() {\n    n.applyPurchase('x');\n  }\n}\n".into(),
            },
            FileSource {
                path: "lib/pro.dart".into(),
                source: "class ProNotifier {\n  ProNotifier();\n  void applyPurchase(String id) {\n  }\n}\n".into(),
            },
        ];
        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        let call = edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls)
            .expect("calls edge");
        assert_eq!(call.source_file, "lib/pay.dart");
        assert!(call.line > 0);
        assert!(call.snippet.contains("applyPurchase"));
        assert_eq!(call.resolver, RESOLVER_DART_LIGHTWEIGHT);
    }

    #[test]
    fn p63_default_options_emit_noise_for_engine_to_filter() {
        // We deliberately ship noise calls upstream so the engine can offer
        // both clean and verbose views from a single index. Verify the
        // adapter does NOT drop a toString call when include_noise is true.
        let target = method("lib/x.dart", "X", "toString", 1, 3);
        let caller = method("lib/y.dart", "Y", "go", 1, 3);
        let symbols = vec![target.clone(), caller.clone()];
        let ranges = vec![range_for(&target), range_for(&caller)];
        let sources = vec![FileSource {
            path: "lib/y.dart".into(),
            source: "class Y {\n  X x = X();\n  void go() { x.toString(); }\n}\n".into(),
        }];
        let edges = compute_references(&sources, &symbols, &ranges, &BTreeMap::new());
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::Calls && e.to_symbol_id == target.id),
            "default ReferenceOptions must emit noise edges; engine filters at view time. got: {edges:?}"
        );
    }

    #[test]
    fn p63_include_noise_false_drops_framework_methods() {
        let target = method("lib/x.dart", "X", "toString", 1, 3);
        let caller = method("lib/y.dart", "Y", "go", 1, 3);
        let symbols = vec![target.clone(), caller.clone()];
        let ranges = vec![range_for(&target), range_for(&caller)];
        let sources = vec![FileSource {
            path: "lib/y.dart".into(),
            source: "class Y {\n  X x = X();\n  void go() { x.toString(); }\n}\n".into(),
        }];
        let edges = compute_references_with_options(
            &sources,
            &symbols,
            &ranges,
            &BTreeMap::new(),
            &ReferenceOptions {
                include_noise: false,
            },
        );
        assert!(
            !edges
                .iter()
                .any(|e| e.kind == EdgeKind::Calls && e.to_symbol_id == target.id),
            "include_noise=false must filter framework methods upstream"
        );
    }

    #[test]
    fn p63_is_framework_noise_recognises_every_declared_name() {
        for name in FRAMEWORK_NOISE_METHODS {
            assert!(is_framework_noise(name), "expected {name} to be noise");
        }
        assert!(!is_framework_noise("applyPurchase"));
        assert!(!is_framework_noise("listenToPurchaseUpdates"));
    }
}
