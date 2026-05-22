## vub (java) — specslice 0.2.0 真实扫描

- 源仓: `/Users/qjs/Code/Demo/vub`
- scratch 副本: `release-scans/_scratch/vub/`（已 gitignore）
- 目标仓副作用: 无 — 没有任何 `.specslice/` / `graph.db` / export 文件落到源仓内。

### `specslice index` 输出

```
Docs index:
  Files: 1
  Requirements: 0
  DocSections: 1
  Edges: 1
Code index:
  Dart files: 0
  Symbols: 0
  TestCases: 0
  Resolver: dart_analyzer
Java index:
  Java files: 3111
  Symbols: 16099
  TestCases: 0
  Imports: 25194
  Resolver: java_ast
  LSP skipped: 未在 PATH 找到 jdtls，已退化为 AST fallback
Links index:
  Requirements: 0
  Docs: 0
  Implementations: 0
  Tests: 0
  Edges: 0
```

### `specslice check` 摘要（前 60 行）

```
SpecSlice Checks: 0 findings.
```

### Graph code-view 规模

- 节点: `18295`
- 边: `40239`

### dead-code (high) 摘要（前 30 行）

```
{
  "schema_version": 1,
  "min_confidence": "high",
  "stats": {
    "total_code_symbols": 15045,
    "entrypoints": 0,
    "reachable": 0,
    "possibly_dead": 0,
    "ignored_by_pattern": 0
  },
  "candidates": []
}

```
