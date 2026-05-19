/// Walks Dart source files with `package:analyzer`'s resolved AST and
/// produces a `LanguageIndexBatch`-shaped JSON. The walker is the
/// "accuracy core" of the SpecSlice Dart adapter — when this is
/// available, the Rust engine prefers its output over the heuristic
/// fallback (`crates/specslice-lang-dart`).
///
/// Design constraints:
/// - Pure analyzer output, no Dart SDK execution.
/// - Same JSON shape as the heuristic adapter so the engine ingest path
///   stays identical.
/// - Resolver tag = "dart_analyzer" on every produced edge.
/// - Resilient to single-file analysis errors: skip the file and emit
///   a `diagnostic` instead of failing the whole batch.
library;

import 'dart:convert';
import 'dart:io';

import 'package:analyzer/dart/analysis/analysis_context_collection.dart';
import 'package:analyzer/dart/analysis/results.dart';
import 'package:analyzer/dart/ast/ast.dart';
import 'package:analyzer/dart/ast/visitor.dart';
import 'package:analyzer/dart/element/element2.dart';
import 'package:crypto/crypto.dart';
import 'package:path/path.dart' as p;

import 'protocol.dart';

class WalkerStats {
  int files = 0;
  int symbols = 0;
  int references = 0;
  int calls = 0;
}

/// Entry point. Returns the JSON-encodable batch.
Future<SidecarBatchResponse> walkRepository(SidecarRequest req) async {
  final repoRoot = req.repoRoot;
  final files = <Map<String, dynamic>>[];
  final symbols = <Map<String, dynamic>>[];
  final symbolRanges = <Map<String, dynamic>>[];
  final imports = <Map<String, dynamic>>[];
  final references = <Map<String, dynamic>>[];
  final diagnostics = <Map<String, dynamic>>[];

  final absoluteRoots = <String>[];
  for (final codeRoot in req.codeRoots) {
    final abs = p.join(repoRoot, codeRoot);
    if (FileSystemEntity.isDirectorySync(abs)) {
      absoluteRoots.add(abs);
    }
  }
  if (absoluteRoots.isEmpty) {
    return SidecarBatchResponse(
      files: files,
      symbols: symbols,
      symbolRanges: symbolRanges,
      imports: imports,
      references: references,
      diagnostics: diagnostics,
    );
  }

  final collection = AnalysisContextCollection(
    includedPaths: absoluteRoots,
    resourceProvider: null,
  );

  // First pass: declarations. We build a "qualified-name → symbol id"
  // map so the second pass can resolve calls/references against the
  // batch's own symbols rather than just opaque elements.
  final byElement = <Element2, String>{};
  final dartFiles = <String>[];
  for (final ctx in collection.contexts) {
    for (final f in ctx.contextRoot.analyzedFiles()) {
      if (!f.endsWith('.dart')) continue;
      // Honour exclude globs (very simple matcher: substring on relative path).
      final relCheck = p.relative(f, from: repoRoot).replaceAll(r'\\', '/');
      if (req.excludeGlobs.any((g) => _matchesGlob(g, relCheck))) {
        continue;
      }
      dartFiles.add(f);
    }
  }
  dartFiles.sort();

  for (final absPath in dartFiles) {
    final rel = p.relative(absPath, from: repoRoot).replaceAll(r'\\', '/');
    final ctx = collection.contextFor(absPath);

    ResolvedUnitResult? unit;
    try {
      final r = await ctx.currentSession.getResolvedUnit(absPath);
      if (r is ResolvedUnitResult) {
        unit = r;
      }
    } catch (e) {
      diagnostics.add({
        'code': 'resolved_unit_failed',
        'severity': 'warning',
        'message': '$absPath: $e',
      });
      continue;
    }
    if (unit == null) {
      diagnostics.add({
        'code': 'resolved_unit_missing',
        'severity': 'warning',
        'message': absPath,
      });
      continue;
    }

    final source = unit.content;
    final hash = sha256.convert(utf8.encode(source)).toString();
    final fileId = 'file::$rel';
    files.add({
      'id': fileId,
      'path': rel,
      'language': 'dart',
      'content_hash': hash,
    });

    // Imports — emit only when the import resolves to a file inside the repo.
    for (final directive in unit.unit.directives) {
      if (directive is ImportDirective) {
        final uri = directive.uri.stringValue;
        if (uri == null) continue;
        final targetAbs = _resolveImportPath(absPath, uri);
        if (targetAbs == null) continue;
        final targetRel =
            p.relative(targetAbs, from: repoRoot).replaceAll(r'\\', '/');
        // Stay inside repo only.
        if (targetRel.startsWith('..')) continue;
        imports.add({
          'from_file': fileId,
          'to_path': targetRel,
        });
      }
    }

    final declared = _DeclarationVisitor(
      rel: rel,
      fileId: fileId,
      lineInfo: unit.lineInfo,
      byElement: byElement,
      symbols: symbols,
      symbolRanges: symbolRanges,
    );
    unit.unit.visitChildren(declared);
  }

  // Second pass: bodies. Walk every method/function/constructor body for
  // calls (MethodInvocation, InstanceCreationExpression) and references
  // (PrefixedIdentifier / SimpleIdentifier resolving to a known
  // declaration), tagging each edge with file:line + snippet.
  for (final absPath in dartFiles) {
    final rel = p.relative(absPath, from: repoRoot).replaceAll(r'\\', '/');
    final ctx = collection.contextFor(absPath);
    ResolvedUnitResult? unit;
    try {
      final r = await ctx.currentSession.getResolvedUnit(absPath);
      if (r is ResolvedUnitResult) unit = r;
    } catch (_) {
      continue;
    }
    if (unit == null) continue;

    final body = _BodyVisitor(
      rel: rel,
      lineInfo: unit.lineInfo,
      source: unit.content,
      byElement: byElement,
      references: references,
    );
    unit.unit.visitChildren(body);
  }

  return SidecarBatchResponse(
    files: files,
    symbols: symbols,
    symbolRanges: symbolRanges,
    imports: imports,
    references: references,
    diagnostics: diagnostics,
  );
}

String? _resolveImportPath(String fromAbs, String uri) {
  // Only resolve relative imports — `package:` and `dart:` cannot be
  // recovered without a full pub workspace and we deliberately keep the
  // sidecar repo-local.
  if (uri.startsWith('dart:') || uri.startsWith('package:')) {
    return null;
  }
  final dir = p.dirname(fromAbs);
  final candidate = p.normalize(p.join(dir, uri));
  if (FileSystemEntity.isFileSync(candidate)) {
    return candidate;
  }
  return null;
}

bool _matchesGlob(String pattern, String path) {
  if (!pattern.contains('*') && !pattern.contains('?')) {
    return path == pattern;
  }
  // Tiny glob: `**` matches any subpath, `*` matches anything except `/`.
  final regex = StringBuffer('^');
  for (int i = 0; i < pattern.length; i++) {
    final ch = pattern[i];
    if (ch == '*') {
      if (i + 1 < pattern.length && pattern[i + 1] == '*') {
        regex.write('.*');
        i++;
      } else {
        regex.write('[^/]*');
      }
    } else if (ch == '?') {
      regex.write('.');
    } else if ('.+()|[]{}^\$'.contains(ch)) {
      regex.write('\\$ch');
    } else {
      regex.write(ch);
    }
  }
  regex.write(r'$');
  return RegExp(regex.toString()).hasMatch(path);
}

/// Walks declarations and builds the symbol table + element → id map.
class _DeclarationVisitor extends RecursiveAstVisitor<void> {
  final String rel;
  final String fileId;
  final dynamic lineInfo; // LineInfo
  final Map<Element2, String> byElement;
  final List<Map<String, dynamic>> symbols;
  final List<Map<String, dynamic>> symbolRanges;

  String? currentClassName;
  String? currentClassId;

  _DeclarationVisitor({
    required this.rel,
    required this.fileId,
    required this.lineInfo,
    required this.byElement,
    required this.symbols,
    required this.symbolRanges,
  });

  @override
  void visitClassDeclaration(ClassDeclaration node) {
    final name = node.name.lexeme;
    final id = 'dart_class::$rel#$name';
    final lines = _lineRange(node.offset, node.end);
    symbols.add({
      'id': id,
      'kind': NodeKindString.dartClass,
      'path': rel,
      'name': name,
      'qualified_name': name,
      'start_line': lines.start,
      'end_line': lines.end,
    });
    symbolRanges.add({
      'file_path': rel,
      'symbol_id': id,
      'start_line': lines.start,
      'end_line': lines.end,
      'symbol_kind': NodeKindString.dartClass,
      'qualified_name': name,
    });
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;

    final prevName = currentClassName;
    final prevId = currentClassId;
    currentClassName = name;
    currentClassId = id;
    super.visitClassDeclaration(node);
    currentClassName = prevName;
    currentClassId = prevId;
  }

  @override
  void visitMethodDeclaration(MethodDeclaration node) {
    final className = currentClassName;
    final classId = currentClassId;
    if (className == null || classId == null) return;
    final name = node.name.lexeme;
    final id = 'dart_method::$rel#$className.$name';
    final lines = _lineRange(node.offset, node.end);
    symbols.add({
      'id': id,
      'kind': NodeKindString.dartMethod,
      'path': rel,
      'name': name,
      'qualified_name': '$className.$name',
      'start_line': lines.start,
      'end_line': lines.end,
      'parent_symbol_id': classId,
    });
    symbolRanges.add({
      'file_path': rel,
      'symbol_id': id,
      'start_line': lines.start,
      'end_line': lines.end,
      'symbol_kind': NodeKindString.dartMethod,
      'qualified_name': '$className.$name',
      'parent_symbol_id': classId,
    });
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;
    super.visitMethodDeclaration(node);
  }

  @override
  void visitConstructorDeclaration(ConstructorDeclaration node) {
    final className = currentClassName;
    final classId = currentClassId;
    if (className == null || classId == null) return;
    final name = node.name?.lexeme ?? '_default';
    final id = 'dart_constructor::$rel#$className.$name';
    final lines = _lineRange(node.offset, node.end);
    symbols.add({
      'id': id,
      'kind': NodeKindString.dartConstructor,
      'path': rel,
      'name': name,
      'qualified_name': '$className.$name',
      'start_line': lines.start,
      'end_line': lines.end,
      'parent_symbol_id': classId,
    });
    symbolRanges.add({
      'file_path': rel,
      'symbol_id': id,
      'start_line': lines.start,
      'end_line': lines.end,
      'symbol_kind': NodeKindString.dartConstructor,
      'qualified_name': '$className.$name',
      'parent_symbol_id': classId,
    });
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;
    super.visitConstructorDeclaration(node);
  }

  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {
    if (currentClassId != null) return; // nested fns inside methods ignored
    final name = node.name.lexeme;
    final id = 'dart_fn::$rel#$name';
    final lines = _lineRange(node.offset, node.end);
    symbols.add({
      'id': id,
      'kind': NodeKindString.dartFunction,
      'path': rel,
      'name': name,
      'qualified_name': name,
      'start_line': lines.start,
      'end_line': lines.end,
    });
    symbolRanges.add({
      'file_path': rel,
      'symbol_id': id,
      'start_line': lines.start,
      'end_line': lines.end,
      'symbol_kind': NodeKindString.dartFunction,
      'qualified_name': name,
    });
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;
    super.visitFunctionDeclaration(node);
  }

  ({int start, int end}) _lineRange(int offsetStart, int offsetEnd) {
    final s = lineInfo.getLocation(offsetStart).lineNumber;
    final e = lineInfo.getLocation(offsetEnd).lineNumber;
    return (start: s, end: e);
  }
}

/// Walks method / function bodies and emits `calls` / `references` edges
/// against the symbol table the declaration pass built.
class _BodyVisitor extends RecursiveAstVisitor<void> {
  final String rel;
  final dynamic lineInfo;
  final String source;
  final Map<Element2, String> byElement;
  final List<Map<String, dynamic>> references;

  String? _currentSymbolId;
  final Set<String> _emitted = <String>{};

  _BodyVisitor({
    required this.rel,
    required this.lineInfo,
    required this.source,
    required this.byElement,
    required this.references,
  });

  @override
  void visitMethodDeclaration(MethodDeclaration node) {
    final el = node.declaredFragment?.element;
    final prev = _currentSymbolId;
    _currentSymbolId = el == null ? null : byElement[el];
    super.visitMethodDeclaration(node);
    _currentSymbolId = prev;
  }

  @override
  void visitConstructorDeclaration(ConstructorDeclaration node) {
    final el = node.declaredFragment?.element;
    final prev = _currentSymbolId;
    _currentSymbolId = el == null ? null : byElement[el];
    super.visitConstructorDeclaration(node);
    _currentSymbolId = prev;
  }

  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {
    final el = node.declaredFragment?.element;
    final prev = _currentSymbolId;
    _currentSymbolId = el == null ? null : byElement[el];
    super.visitFunctionDeclaration(node);
    _currentSymbolId = prev;
  }

  @override
  void visitMethodInvocation(MethodInvocation node) {
    final from = _currentSymbolId;
    final target = node.methodName.element;
    if (from != null && target != null) {
      final to = byElement[target];
      if (to != null) {
        _emit(from, to, EdgeKindString.calls, node.offset, node.end);
      }
    }
    super.visitMethodInvocation(node);
  }

  @override
  void visitInstanceCreationExpression(InstanceCreationExpression node) {
    final from = _currentSymbolId;
    final ctor = node.constructorName.element;
    if (from != null && ctor != null) {
      final to = byElement[ctor];
      if (to != null) {
        _emit(from, to, EdgeKindString.calls, node.offset, node.end);
      } else {
        // Constructor wasn't itself a tracked symbol — fall back to its
        // enclosing class so the edge still appears in the graph.
        final enclosing = ctor.enclosingElement2;
        if (enclosing != null) {
          final classId = byElement[enclosing];
          if (classId != null) {
            _emit(from, classId, EdgeKindString.references, node.offset,
                node.end);
          }
        }
      }
    }
    super.visitInstanceCreationExpression(node);
  }

  @override
  void visitPrefixedIdentifier(PrefixedIdentifier node) {
    final from = _currentSymbolId;
    final el = node.prefix.element;
    if (from != null && el != null) {
      final to = byElement[el];
      if (to != null) {
        _emit(from, to, EdgeKindString.references, node.offset, node.end);
      }
    }
    super.visitPrefixedIdentifier(node);
  }

  @override
  void visitSimpleIdentifier(SimpleIdentifier node) {
    // Avoid double-counting: simple identifiers inside a
    // MethodInvocation / PrefixedIdentifier are reported by the more
    // specific visitor.
    if (node.parent is MethodInvocation || node.parent is PrefixedIdentifier) {
      return;
    }
    final from = _currentSymbolId;
    final el = node.element;
    if (from != null && el != null) {
      final to = byElement[el];
      if (to != null) {
        _emit(from, to, EdgeKindString.references, node.offset, node.end);
      }
    }
    super.visitSimpleIdentifier(node);
  }

  void _emit(String from, String to, String kind, int offset, int end) {
    if (from == to) return;
    final dedupKey = '$from|$to|$kind';
    if (!_emitted.add(dedupKey)) return;
    final lineNo = lineInfo.getLocation(offset).lineNumber as int;
    references.add({
      'from_symbol_id': from,
      'to_symbol_id': to,
      'kind': kind,
      'source_file': rel,
      'line': lineNo,
      'snippet': _snippetAround(offset, end),
      'resolver': resolverDartAnalyzer,
    });
  }

  String _snippetAround(int offset, int end) {
    // Grab the entire source line containing `offset` — keeps the snippet
    // user-meaningful (a full statement) and consistent with the
    // heuristic adapter's output.
    final lineNo = lineInfo.getLocation(offset).lineNumber as int;
    final start = lineInfo.getOffsetOfLine(lineNo - 1);
    int stop;
    try {
      stop = lineInfo.getOffsetOfLine(lineNo);
    } catch (_) {
      stop = source.length;
    }
    final raw = source.substring(start, stop).trim();
    if (raw.length <= 200) return raw;
    return '${raw.substring(0, 200)}…';
  }
}
