//! P25 — totality & determinism for the hand-rolled *behavioural fact* scanners
//! (literal catalogue, noise stripping, identifier tokenisation, SQL / MyBatis
//! schema parsing).
//!
//! The tree-sitter backends are fuzzed end-to-end by p23. These scanners are
//! different: they walk the source by hand — `strip_noise` / `scan_literals`
//! over a `Vec<char>`, and the schema parsers over raw **byte offsets** found
//! via substring search. A mishandled multi-byte UTF-8 boundary or an
//! unbalanced quote/comment could panic (`byte index N is not a char
//! boundary`) or drift between runs. Each scanner must:
//!   * never panic on arbitrary UTF-8 input,
//!   * be deterministic (same input → same output), and
//!   * `strip_noise` must additionally preserve the newline count — its callers
//!     map line numbers through the stripped text, so a swallowed `\n` would
//!     silently misattribute every downstream fact.

use proptest::prelude::*;
use specslice_core::language_traits::Language;
use specslice_engine::constants::scan_literals;
use specslice_engine::fts_text::fts_tokens;
use specslice_engine::schema_indexer::{
    extract_sql_table_refs, parse_dart_consumed_calls, parse_dart_route_constants,
    parse_gin_routes, parse_go_routes, parse_http_routes, parse_mapper_stmts, parse_python_routes,
    parse_sql_tables, parse_ts_consumed_calls, parse_ts_server_routes,
};
use specslice_engine::source_text::{identifier_tokens, strip_noise};

/// The host languages whose source the hand-rolled scanners run on.
const CODE_LANGS: &[Language] = &[
    Language::Dart,
    Language::Swift,
    Language::Go,
    Language::Python,
    Language::Typescript,
    Language::Java,
    Language::Rust,
    Language::C,
    Language::Cpp,
];

/// Tokens chosen to drive the scanners' state machines through every branch:
/// comment openers/closers, every quote flavour, escapes, hash comments, plus
/// multi-byte UTF-8 (2/3/4-byte) that lands on arbitrary offsets to stress char
/// boundaries, and SQL / MyBatis-XML surface for the schema parsers.
const TOKENS: &[&str] = &[
    // comment + quote + escape machinery
    "//",
    "/*",
    "*/",
    "#",
    "\"",
    "'",
    "`",
    "\"\"\"",
    "'''",
    "\\",
    "\\\"",
    "\n",
    "\r\n",
    "\t",
    " ",
    // punctuation / identifiers
    "(",
    ")",
    ",",
    ";",
    "{",
    "}",
    "=",
    ".",
    "a",
    "id",
    "value",
    // multi-byte UTF-8 boundary stressors
    "é",
    "—",
    "你好",
    "🚀",
    "Ω",
    "ä",
    "𝟙",
    // SQL surface
    "create table",
    "CREATE TABLE",
    "from ",
    "JOIN ",
    "into ",
    "select ",
    "where ",
    "`tbl`",
    "\"sch\".\"t\"",
    "users",
    "(id int)",
    // paren bodies that END on a multi-byte char — the shape that crashed the
    // SQL table scanner (`search_from` advanced one byte before `)`, mid-char).
    "(é)",
    "(你好)",
    "(col …)",
    "(x 🚀)",
    // MyBatis XML surface
    "<select id=\"x\">",
    "</select>",
    "<insert>",
    "</insert>",
    "<update>",
    "<delete>",
    "<mapper namespace=\"a\">",
    "</mapper>",
    "${p}",
    "#{p}",
    "<![CDATA[",
    "]]>",
    // HTTP route / consumed-call surface (Spring / Gin / Express / FastAPI / Dart)
    "@GetMapping(\"/x\")",
    "@RequestMapping",
    "@PostMapping(",
    "@RestController",
    "r.GET(\"/p\",",
    "router.get('/p',",
    "app.post(\"/p\")",
    "@app.route('/p')",
    "fetch('/api/x')",
    "http.get(",
    "Uri.parse(\"/p\")",
    "\"/api/v1/users\"",
];

/// Concatenate a random sequence of the interesting tokens into one source.
fn fragment() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(TOKENS), 0..48).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `scan_literals` is total + deterministic for every host language.
    #[test]
    fn scan_literals_is_total_and_deterministic(s in fragment()) {
        for &lang in CODE_LANGS {
            let a = scan_literals(&s, lang);
            let b = scan_literals(&s, lang);
            prop_assert_eq!(a.len(), b.len(), "scan_literals nondeterministic for {:?}", lang);
        }
    }

    /// `strip_noise` is total + deterministic *and* preserves the newline count.
    #[test]
    fn strip_noise_is_total_deterministic_and_newline_preserving(s in fragment()) {
        let newlines = s.matches('\n').count();
        for &lang in CODE_LANGS {
            let a = strip_noise(&s, lang);
            let b = strip_noise(&s, lang);
            prop_assert_eq!(&a, &b, "strip_noise nondeterministic for {:?}", lang);
            prop_assert_eq!(
                a.matches('\n').count(),
                newlines,
                "strip_noise changed the newline count for {:?}",
                lang
            );
        }
    }

    /// `identifier_tokens` is total + deterministic (it byte-slices the source,
    /// so returning the tokens at all proves it never sliced mid-char).
    #[test]
    fn identifier_tokens_is_total_and_deterministic(s in fragment()) {
        let a = identifier_tokens(&s);
        let b = identifier_tokens(&s);
        prop_assert_eq!(a, b);
    }

    /// The byte-offset SQL / MyBatis-XML scanners are total + deterministic on
    /// arbitrary UTF-8 — they slice the source at byte offsets located via
    /// substring search, so they are the real char-boundary risk.
    #[test]
    fn schema_scanners_are_total_and_deterministic(s in fragment()) {
        prop_assert_eq!(parse_mapper_stmts(&s).len(), parse_mapper_stmts(&s).len());
        prop_assert_eq!(parse_sql_tables(&s).len(), parse_sql_tables(&s).len());
        prop_assert_eq!(extract_sql_table_refs(&s).len(), extract_sql_table_refs(&s).len());
    }

    /// The bilingual FTS tokenizer feeds every body and every query of the
    /// content layer — it must be total + deterministic on arbitrary UTF-8,
    /// and must never emit a token containing `"` (which would break the FTS5
    /// MATCH expression quoting).
    #[test]
    fn fts_tokenizer_is_total_deterministic_and_quote_free(s in fragment()) {
        let a = fts_tokens(&s);
        prop_assert_eq!(&a, &fts_tokens(&s));
        prop_assert!(a.iter().all(|t| !t.contains('"')));
    }

    /// The byte-offset HTTP route / consumed-call scanners (Spring / Go / Gin /
    /// Express-TS / FastAPI / Dart) are total + deterministic on arbitrary UTF-8.
    #[test]
    fn route_scanners_are_total_and_deterministic(s in fragment()) {
        prop_assert_eq!(parse_http_routes(&s).len(), parse_http_routes(&s).len());
        prop_assert_eq!(parse_go_routes(&s).len(), parse_go_routes(&s).len());
        prop_assert_eq!(parse_gin_routes(&s).len(), parse_gin_routes(&s).len());
        prop_assert_eq!(parse_ts_server_routes(&s).len(), parse_ts_server_routes(&s).len());
        prop_assert_eq!(parse_python_routes(&s).len(), parse_python_routes(&s).len());
        prop_assert_eq!(
            parse_dart_route_constants(&s).len(),
            parse_dart_route_constants(&s).len()
        );
        prop_assert_eq!(
            parse_dart_consumed_calls(&s).len(),
            parse_dart_consumed_calls(&s).len()
        );
        prop_assert_eq!(parse_ts_consumed_calls(&s).len(), parse_ts_consumed_calls(&s).len());
    }
}
