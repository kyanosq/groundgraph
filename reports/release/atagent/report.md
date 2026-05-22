## atagent (python) — specslice 0.2.0 真实扫描

- 源仓: `/Users/qjs/Code/Projects/atagent`
- scratch 副本: `release-scans/_scratch/atagent/`（已 gitignore）
- 目标仓副作用: 无 — 没有任何 `.specslice/` / `graph.db` / export 文件落到源仓内。

### `specslice index` 输出

```
Docs index:
  Files: 9
  Requirements: 0
  DocSections: 181
  Edges: 181
Code index:
  Dart files: 0
  Symbols: 0
  TestCases: 0
  Resolver: dart_analyzer
Python index:
  Python files: 165
  Symbols: 1224
  TestCases: 272
  Imports: 665
  Framework entrypoints: 45
  Resolver: python_ast
  LSP skipped: 未在 PATH / .venv 中找到可启动的 pyright/basedpyright/pylsp（要么不存在，要么 `--help` 启动失败），已退化为 AST fallback
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

- 节点: `1807`
- 边: `2054`

### dead-code (high) 摘要（前 30 行）

```
{
  "schema_version": 1,
  "min_confidence": "high",
  "stats": {
    "total_code_symbols": 1224,
    "entrypoints": 407,
    "reachable": 458,
    "possibly_dead": 203,
    "ignored_by_pattern": 0
  },
  "candidates": [
    {
      "id": "python::backend/app/admin/cli.py::_alembic_cfg",
      "kind": "python_function",
      "label": "_alembic_cfg",
      "path": "backend/app/admin/cli.py",
      "line_range": [
        7,
        13
      ],
      "confidence": "high",
      "reasons": [
        "未被 Python 入口（main / app / pytest / dunder / 公开 API）任一入口点可达",
        "无任何 calls / references / declares_verification 入边"
      ],
      "inbound_sources": []
    },
    {
      "id": "python::backend/app/api/v1/endpoints/callbacks.py::_handle_callback",
      "kind": "python_function",

```
