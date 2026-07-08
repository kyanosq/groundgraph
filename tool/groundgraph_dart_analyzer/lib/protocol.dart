/// JSON shapes shared between the Rust engine and the Dart sidecar.
///
/// The protocol is request-response over a single subprocess invocation:
///
/// - **Request**: a JSON object on `stdin` describing the repo and which
///   code roots to scan. See [SidecarRequest].
/// - **Response**: a single JSON object on `stdout`. On success the
///   payload is a [SidecarBatchResponse]; on a recoverable failure it is
///   a [SidecarErrorResponse]. Mismatched shapes always count as
///   failure on the Rust side.
///
/// The batch shape mirrors `groundgraph_core::language_batch::LanguageIndexBatch`
/// so the Rust engine can ingest sidecar output through the same code
/// path it uses for the heuristic adapter.
library;

const String resolverDartAnalyzer = 'dart_analyzer';

class SidecarRequest {
  final String repoRoot;
  final List<String> codeRoots;
  final List<String> excludeGlobs;
  final bool resolveImports;

  SidecarRequest({
    required this.repoRoot,
    required this.codeRoots,
    this.excludeGlobs = const [],
    this.resolveImports = true,
  });

  factory SidecarRequest.fromJson(Map<String, dynamic> json) {
    return SidecarRequest(
      repoRoot: json['repo_root'] as String,
      codeRoots: (json['code_roots'] as List<dynamic>)
          .map((e) => e as String)
          .toList(),
      excludeGlobs: (json['exclude_globs'] as List<dynamic>?)
              ?.map((e) => e as String)
              .toList() ??
          const [],
      resolveImports: json['resolve_imports'] as bool? ?? true,
    );
  }
}

/// Same shape as `groundgraph_core::EdgeKind` (snake_case).
abstract class EdgeKindString {
  static const contains = 'contains';
  static const imports = 'imports';
  static const references = 'references';
  static const calls = 'calls';
  // P8 — framework-aware semantic edges.
  static const readsProvider = 'reads_provider';
  static const navigatesTo = 'navigates_to';
  static const persistsTo = 'persists_to';
  static const subscribesStream = 'subscribes_stream';
}

/// Same shape as `groundgraph_core::NodeKind` (snake_case).
abstract class NodeKindString {
  static const file = 'file';
  static const dartClass = 'dart_class';
  static const dartMethod = 'dart_method';
  static const dartFunction = 'dart_function';
  static const dartConstructor = 'dart_constructor';
  static const testCase = 'test_case';
  static const testGroup = 'test_group';
  // P8 — synthetic target node kinds.
  static const dartProvider = 'dart_provider';
  static const route = 'route';
  static const storage = 'storage';
}

class SidecarBatchResponse {
  final List<Map<String, dynamic>> files;
  final List<Map<String, dynamic>> symbols;
  final List<Map<String, dynamic>> tests;
  final List<Map<String, dynamic>> symbolRanges;
  final List<Map<String, dynamic>> imports;
  final List<Map<String, dynamic>> references;
  final List<Map<String, dynamic>> syntheticNodes;
  final List<Map<String, dynamic>> diagnostics;

  SidecarBatchResponse({
    required this.files,
    required this.symbols,
    required this.tests,
    required this.symbolRanges,
    required this.imports,
    required this.references,
    required this.syntheticNodes,
    required this.diagnostics,
  });

  Map<String, dynamic> toJson() => {
        'ok': true,
        'resolver': resolverDartAnalyzer,
        'files': files,
        'symbols': symbols,
        'tests': tests,
        'symbol_ranges': symbolRanges,
        'imports': imports,
        'references': references,
        'synthetic_nodes': syntheticNodes,
        'diagnostics': diagnostics,
      };
}

class SidecarErrorResponse {
  final String code;
  final String message;
  final String? detail;

  SidecarErrorResponse({
    required this.code,
    required this.message,
    this.detail,
  });

  Map<String, dynamic> toJson() => {
        'ok': false,
        'error_code': code,
        'error_message': message,
        if (detail != null) 'detail': detail,
      };
}
