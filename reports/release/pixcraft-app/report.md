## pixcraft-app (dart) — specslice 0.2.0 真实扫描

- 源仓: `/Users/qjs/Code/My/bean/pixcraft-app`
- scratch 副本: `release-scans/_scratch/pixcraft-app/`（已 gitignore）
- 目标仓副作用: 无 — 没有任何 `.specslice/` / `graph.db` / export 文件落到源仓内。

### `specslice index` 输出

```
Docs index:
  Files: 2
  Requirements: 0
  DocSections: 71
  Edges: 71
Code index:
  Dart files: 151
  Symbols: 6964
  TestCases: 366
  Resolver: dart_analyzer
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

- 节点: `7653`
- 边: `8869`

### dead-code (high) 摘要（前 30 行）

```
{
  "schema_version": 1,
  "min_confidence": "high",
  "stats": {
    "total_code_symbols": 6875,
    "entrypoints": 692,
    "reachable": 1651,
    "possibly_dead": 6,
    "ignored_by_pattern": 4971
  },
  "candidates": [
    {
      "id": "dart_method::lib/core/utils/background_removal_service.dart#BackgroundRemovalService._removeBackgroundOHOS",
      "kind": "dart_method",
      "label": "_removeBackgroundOHOS",
      "path": "lib/core/utils/background_removal_service.dart",
      "line_range": [
        101,
        176
      ],
      "confidence": "high",
      "reasons": [
        "未被 main / 路由 / Provider / 测试 / lifecycle 任一入口点可达",
        "无任何 calls / references / declares_verification 入边"
      ],
      "inbound_sources": []
    },
    {
      "id": "dart_method::lib/core/utils/project_schema.dart#ProjectSchema._migrateV1ToV2",
      "kind": "dart_method",

```
