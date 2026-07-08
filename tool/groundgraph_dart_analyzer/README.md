# groundgraph_dart_analyzer (P7 sidecar)

A Dart subprocess that uses `package:analyzer` to produce a resolved-AST
`LanguageIndexBatch` for the GroundGraph Rust engine. When this sidecar is
available, the engine prefers its output over the lightweight heuristic
adapter shipped at `crates/groundgraph-lang-dart/`.

## Why a sidecar

`package:analyzer` is the only source of truth for Dart's resolved AST
(element resolution, generics, extension methods, import aliases,
mixins, augmentations, package URIs). Re-implementing element
resolution in Rust would always lag upstream. Running it out-of-process
keeps the Rust engine analyzer-agnostic.

## Enabling

Set `GROUNDGRAPH_DART_ANALYZER=1` before running `groundgraph index`. The
engine looks for the sidecar at:

1. `$GROUNDGRAPH_DART_ANALYZER_BIN` (a shell-style command, e.g.
   `dart run /path/to/bin/groundgraph_dart_analyzer.dart`).
2. Otherwise: `dart run tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart`
   resolved against the repo root.

If neither is available (Dart SDK not installed, sidecar source missing,
JSON malformed, etc.), the engine falls back to the heuristic adapter
silently. The `DartIndexResult.sidecar_skip_reason` field carries the
explanation.

## Protocol

```jsonc
// stdin
{
  "repo_root": "/abs/path/to/repo",
  "code_roots": ["lib", "test"],
  "exclude_globs": ["**/*.g.dart"],
  "resolve_imports": true
}

// stdout — success
{
  "ok": true,
  "resolver": "dart_analyzer",
  "files": [...],
  "symbols": [...],
  "symbol_ranges": [...],
  "imports": [...],
  "references": [...],
  "diagnostics": [...]
}

// stdout — recoverable failure
{ "ok": false, "error_code": "...", "error_message": "..." }
```

Every entry in `references` is one of:

- `kind: "calls"` — a method invocation / constructor call whose target
  resolves to a known symbol.
- `kind: "references"` — a class / constant / extension reference.

Each entry carries `source_file`, `line`, `snippet`, and
`resolver: "dart_analyzer"`.

## Quick test

```bash
cd tool/groundgraph_dart_analyzer
dart pub get
echo '{"repo_root":"/path/to/repo","code_roots":["lib"]}' \
  | dart run bin/groundgraph_dart_analyzer.dart \
  | jq '.references[0]'
```
