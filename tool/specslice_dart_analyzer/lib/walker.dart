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

import 'package:analyzer/dart/analysis/results.dart';
import 'package:analyzer/dart/ast/ast.dart';
import 'package:analyzer/dart/ast/visitor.dart';
import 'package:analyzer/dart/element/element2.dart';
// ignore: implementation_imports — only the impl constructor accepts
// `optionsFile`, which we need to override the target project's lint-time
// `analyzer: exclude:` so graph coverage follows the code roots, not the
// project's lint scope (see the collection construction below).
import 'package:analyzer/src/dart/analysis/analysis_context_collection.dart';
import 'package:crypto/crypto.dart';
import 'package:path/path.dart' as p;

import 'protocol.dart';

class WalkerStats {
  int files = 0;
  int symbols = 0;
  int references = 0;
  int calls = 0;
}

/// True when [dir] is the root of a usable Dart SDK — i.e. it contains the
/// `sdk_library_metadata/libraries.dart` file `package:analyzer` reads first
/// when building its SDK model. This is exactly the file whose absence
/// produced `PathNotFoundException(.../lib/_internal/.../libraries.dart)` when
/// the sidecar ran as an AOT-compiled executable with a bogus default SDK
/// path.
bool isValidSdk(String dir) {
  if (dir.isEmpty) return false;
  return File(p.join(
    dir,
    'lib',
    '_internal',
    'sdk_library_metadata',
    'lib',
    'libraries.dart',
  )).existsSync();
}

/// Resolve a Dart SDK root that works in *both* sidecar deployment modes:
///
/// - `dart run …` — `package:analyzer`'s own default
///   (`dirname(dirname(Platform.resolvedExecutable))`) already points at the
///   SDK, so we return `null` and let it auto-detect (no behaviour change).
/// - AOT-compiled binary (`SPECSLICE_DART_ANALYZER_BIN=/path/to/exe`) —
///   `resolvedExecutable` is the binary itself, so the default resolves to a
///   bogus path (e.g. `/tmp/lib/_internal/…`) and analysis crashes. We then
///   honour `SPECSLICE_DART_SDK` / `DART_SDK`, or locate `dart` on `PATH` and
///   derive the SDK (handling a plain SDK at `<sdk>/bin/dart` and a Flutter
///   checkout whose SDK lives at `<flutter>/bin/cache/dart-sdk`).
///
/// Returns `null` when the analyzer default already works or nothing better is
/// found (in which case the analyzer keeps its current behaviour).
///
/// [resolvedExecutable] and [environment] are injectable for testing.
String? resolveSdkPath({String? resolvedExecutable, Map<String, String>? environment}) {
  final exe = resolvedExecutable ?? Platform.resolvedExecutable;
  final env = environment ?? Platform.environment;

  // The analyzer's own default. If it works, don't override it.
  final fromExe = p.dirname(p.dirname(exe));
  if (isValidSdk(fromExe)) return null;

  // Explicit overrides win.
  for (final key in const ['SPECSLICE_DART_SDK', 'DART_SDK']) {
    final v = env[key];
    if (v != null && isValidSdk(v)) return v;
  }

  // Locate `dart` on PATH and derive the SDK root.
  final exeName = Platform.isWindows ? 'dart.exe' : 'dart';
  final sep = Platform.isWindows ? ';' : ':';
  for (final dir in (env['PATH'] ?? '').split(sep)) {
    if (dir.isEmpty) continue;
    final dart = p.join(dir, exeName);
    if (!File(dart).existsSync()) continue;
    var real = dart;
    try {
      real = File(dart).resolveSymbolicLinksSync();
    } catch (_) {
      // Keep the unresolved path; symlink resolution is best-effort.
    }
    final candidates = [
      p.dirname(p.dirname(real)), // <sdk>/bin/dart        → <sdk>
      p.join(p.dirname(real), 'cache', 'dart-sdk'), // <flutter>/bin/dart → cache/dart-sdk
    ];
    for (final c in candidates) {
      if (isValidSdk(c)) return c;
    }
  }
  return null;
}

/// Entry point. Returns the JSON-encodable batch.
Future<SidecarBatchResponse> walkRepository(SidecarRequest req) async {
  // `package:analyzer` insists on absolute, normalised paths — relative
  // paths (e.g. `.` from `specslice --repo-root .`) throw inside the
  // context-collection constructor. Normalise once at the entry so every
  // downstream `p.join(...)` is happy.
  final repoRoot = p.normalize(p.absolute(req.repoRoot));
  final files = <Map<String, dynamic>>[];
  final symbols = <Map<String, dynamic>>[];
  final tests = <Map<String, dynamic>>[];
  final symbolRanges = <Map<String, dynamic>>[];
  final imports = <Map<String, dynamic>>[];
  final references = <Map<String, dynamic>>[];
  // P8 — synthetic target nodes (routes / storage buckets) that the body
  // visitor materialises on demand and de-duplicates by id.
  final syntheticNodes = <String, Map<String, dynamic>>{};
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
      tests: tests,
      symbolRanges: symbolRanges,
      imports: imports,
      references: references,
      syntheticNodes: syntheticNodes.values.toList(),
      diagnostics: diagnostics,
    );
  }

  // Override the target project's analysis options with a minimal, empty
  // one. The project's `analysis_options.yaml` frequently excludes generated
  // code from the *linter*, e.g.
  //   analyzer:
  //     exclude:
  //       - lib/l10n/generated/**
  // `analyzedFiles()` (and `contextFor`) honour that exclude, so those files
  // would resolve to *zero* symbols/edges here — while the tree-sitter
  // structural pass still indexes them (they are real code that runs). That
  // asymmetry strands generated classes with structural nodes but no semantic
  // inbound edges, surfacing them as high-confidence dead code (dogfood: hama
  // l10n `_AppLocalizationsDelegate`). Graph coverage must follow the code
  // roots, not the project's lint scope — SpecSlice has its own exclude config
  // for graph scoping (honoured below via `req.excludeGlobs`). Forcing an
  // empty options file drops the lint-time excludes (and lints, which we never
  // consume) while package resolution + language version still come from the
  // project's package config.
  final optionsDir = await Directory.systemTemp.createTemp('specslice_opts_');
  final optionsFile = File(p.join(optionsDir.path, 'analysis_options.yaml'))
    ..writeAsStringSync('analyzer:\n');

  // P2 — pass the repo root as a single included path so the analyzer
  // treats the whole repo as one context. Otherwise (without a
  // pubspec.yaml) it spawns a separate context per directory, and
  // cross-directory element identity (e.g. `IapProductIds` referenced
  // from `test/` and declared in `lib/`) breaks: the byElement lookup
  // returns null because the analyzer resynthesises the class under a
  // different Element2 instance per context. We still honour the
  // requested code roots when *enumerating* files to index.
  final collection = AnalysisContextCollectionImpl(
    includedPaths: [repoRoot],
    optionsFile: optionsFile.path,
    // null → analyzer auto-detects (correct under `dart run`); non-null →
    // an SDK we resolved for the AOT-compiled-binary deployment (see
    // [resolveSdkPath]).
    sdkPath: resolveSdkPath(),
  );

  // First pass: declarations. We build a "qualified-name → symbol id"
  // map so the second pass can resolve calls/references against the
  // batch's own symbols rather than just opaque elements.
  final byElement = <Element2, String>{};
  // Providers also accessed via their synthetic getter element — Riverpod
  // usage sites resolve `proProvider` to a getter, not to the variable.
  // We keep a per-file name-based lookup so [_resolveProviderId] can fall
  // back when the getter element isn't in [byElement].
  final providerByFileAndName = <String, String>{};
  final constStringByFileAndName = <String, String>{};
  final dartFiles = <String>[];
  for (final ctx in collection.contexts) {
    for (final f in ctx.contextRoot.analyzedFiles()) {
      if (!f.endsWith('.dart')) continue;
      // Restrict to files under one of the requested code roots — the
      // analysis context covers the whole repo (for element identity),
      // but we only want to *emit* nodes/edges for the user-selected
      // roots.
      final inRoot = absoluteRoots
          .any((root) => p.isWithin(root, f) || p.equals(root, f));
      if (!inRoot) continue;
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
      providerByFileAndName: providerByFileAndName,
      constStringByFileAndName: constStringByFileAndName,
    );
    unit.unit.visitChildren(declared);

    if (_isTestSourcePath(rel)) {
      final testDeclarations = _TestDeclarationVisitor(
        rel: rel,
        lineInfo: unit.lineInfo,
        tests: tests,
      );
      unit.unit.visitChildren(testDeclarations);
    }
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
      providerByFileAndName: providerByFileAndName,
      constStringByFileAndName: constStringByFileAndName,
      references: references,
      syntheticNodes: syntheticNodes,
    );
    unit.unit.visitChildren(body);
  }

  // The analyzer has read the override options file by now; drop the temp dir.
  try {
    optionsDir.deleteSync(recursive: true);
  } catch (_) {
    // Best-effort cleanup; a leftover temp file is harmless.
  }

  return SidecarBatchResponse(
    files: files,
    symbols: symbols,
    tests: tests,
    symbolRanges: symbolRanges,
    imports: imports,
    references: references,
    syntheticNodes: syntheticNodes.values.toList(),
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

bool _isTestSourcePath(String rel) {
  return rel.startsWith('test/') ||
      rel.startsWith('integration_test/') ||
      rel.endsWith('_test.dart');
}

/// Slugify mirrors the test-declaration formula: lowercase ascii
/// letters & digits, every other character collapses to a single
/// dash; leading/trailing dashes are trimmed. Kept as a single
/// top-level helper so the declaration pass and the body pass produce
/// identical synthetic ids for the same `test('name', ...)`.
String _slugifyTestName(String value) {
  final buf = StringBuffer();
  var lastWasDash = false;
  for (final codeUnit in value.toLowerCase().codeUnits) {
    final isAsciiLetter = codeUnit >= 97 && codeUnit <= 122;
    final isDigit = codeUnit >= 48 && codeUnit <= 57;
    if (isAsciiLetter || isDigit) {
      buf.writeCharCode(codeUnit);
      lastWasDash = false;
    } else if (!lastWasDash) {
      buf.write('-');
      lastWasDash = true;
    }
  }
  final slug = buf.toString().replaceAll(RegExp(r'^-+|-+$'), '');
  return slug.isEmpty ? 'unnamed' : slug;
}

/// The simple name of an extension's `on` type, matching the tree-sitter
/// indexer's qualifier (generics + library prefixes stripped):
/// `extension _X on Foo<T>` → `Foo`; `extension _X on a.B` → `B`.
String? _extensionOnTypeName(ExtensionDeclaration node) {
  final extended = node.onClause?.extendedType;
  if (extended == null) return null;
  var s = extended.toSource();
  final lt = s.indexOf('<');
  if (lt >= 0) s = s.substring(0, lt);
  final dot = s.lastIndexOf('.');
  if (dot >= 0) s = s.substring(dot + 1);
  s = s.trim();
  return s.isEmpty ? null : s;
}

/// Walks declarations and builds the symbol table + element → id map.
class _DeclarationVisitor extends RecursiveAstVisitor<void> {
  final String rel;
  final String fileId;
  final dynamic lineInfo; // LineInfo
  final Map<Element2, String> byElement;
  final List<Map<String, dynamic>> symbols;
  final List<Map<String, dynamic>> symbolRanges;
  final Map<String, String> providerByFileAndName;
  final Map<String, String> constStringByFileAndName;

  String? currentClassName;
  String? currentClassId;

  _DeclarationVisitor({
    required this.rel,
    required this.fileId,
    required this.lineInfo,
    required this.byElement,
    required this.symbols,
    required this.symbolRanges,
    required this.providerByFileAndName,
    required this.constStringByFileAndName,
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
  void visitMixinDeclaration(MixinDeclaration node) {
    // Mixins (and enums below) collapse onto the `dart_class::` id scheme
    // to mirror the tree-sitter structural pass. The declaration pass only
    // needs to register their Element → id mapping (and descend so their
    // members map too) — the *nodes* are owned by tree-sitter — so usage
    // edges (e.g. a `with _Mixin` application) resolve to a real symbol.
    final name = node.name.lexeme;
    final id = 'dart_class::$rel#$name';
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;
    final prevName = currentClassName;
    final prevId = currentClassId;
    currentClassName = name;
    currentClassId = id;
    super.visitMixinDeclaration(node);
    currentClassName = prevName;
    currentClassId = prevId;
  }

  @override
  void visitEnumDeclaration(EnumDeclaration node) {
    final name = node.name.lexeme;
    final id = 'dart_class::$rel#$name';
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;
    final prevName = currentClassName;
    final prevId = currentClassId;
    currentClassName = name;
    currentClassId = id;
    super.visitEnumDeclaration(node);
    currentClassName = prevName;
    currentClassId = prevId;
  }

  @override
  void visitExtensionDeclaration(ExtensionDeclaration node) {
    // `extension _X on _State { … }` — bind members to the `on` type so
    // their ids match the tree-sitter pass (`dart_method::<file>#_State.m`).
    // Without this, every private extension member is unreachable in the
    // graph and surfaces as a high-confidence dead-code false positive
    // (dogfood regression from tailorx's part-file extension helpers).
    final onType = _extensionOnTypeName(node);
    if (onType == null) {
      super.visitExtensionDeclaration(node);
      return;
    }
    final prevName = currentClassName;
    final prevId = currentClassId;
    currentClassName = onType;
    // The `on` type may be declared in another file, so we cannot point at a
    // real class node id. The synthetic `dart_extension::…` parent is enough:
    // `visitMethodDeclaration` qualifies members under `onType` and the engine
    // re-homes them onto the file when this parent is absent (see
    // `reconcile_misparsed_callables` / `backfill_referenced_symbols`).
    currentClassId = 'dart_extension::$rel#$onType';
    super.visitExtensionDeclaration(node);
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
    final el = node.declaredFragment?.element;
    if (el != null) byElement[el] = id;
    // Emit a symbol row for *every* method, including extension members.
    //
    // The tree-sitter structural pass normally owns these nodes, and when it
    // parses cleanly the engine drops this duplicate (it only pulls an overlay
    // symbol into the graph via `reconcile_misparsed_callables` /
    // `backfill_referenced_symbols`, both of which skip ids already present —
    // so no clobber). But when tree-sitter mis-parses a file (Dart 3 syntax it
    // cannot handle yields a cascade of ERROR nodes — dogfood: turing's
    // `game_screen_editor.dart`), the extension member degrades into a phantom
    // top-level `dart_fn`, every analyzer usage edge targets the real
    // `dart_method::<file>#OnType.member` id, and the member surfaces as a
    // high-confidence dead-code false positive. Emitting it here lets the
    // engine reconcile the phantom away and resolve those edges.
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
    super.visitMethodDeclaration(node);
  }

  @override
  void visitConstructorDeclaration(ConstructorDeclaration node) {
    final className = currentClassName;
    final classId = currentClassId;
    if (className == null || classId == null) return;
    // Address constructors with the canonical `dart_ctor::path#Class.<default>`
    // id scheme — the same one the tree-sitter structural pass and
    // `dart_constructor_id` use. The sidecar previously emitted
    // `dart_constructor::path#Class._default`, which matched neither the
    // structural node nor the backfill predicate, so every construction edge
    // dangled and freshly-constructed classes looked dead (dogfood: hama).
    final ctorName = node.name?.lexeme;
    final suffix =
        (ctorName == null || ctorName.isEmpty) ? '<default>' : ctorName;
    final id = 'dart_ctor::$rel#$className.$suffix';
    final qualifiedName = '$className.$suffix';
    final lines = _lineRange(node.offset, node.end);
    symbols.add({
      'id': id,
      'kind': NodeKindString.dartConstructor,
      'path': rel,
      'name': suffix,
      'qualified_name': qualifiedName,
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
      'qualified_name': qualifiedName,
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

  @override
  void visitFieldDeclaration(FieldDeclaration node) {
    _recordConstStringVariables(node.fields);
    super.visitFieldDeclaration(node);
  }

  /// P8 — collect top-level Riverpod-shaped providers as `dart_provider`
  /// symbols. We detect them by initializer type name: a top-level
  /// `final foo = Provider(...)` or `StateNotifierProvider(...)` /
  /// `ChangeNotifierProvider(...)` / `AutoDisposeProvider(...)` /
  /// `FutureProvider(...)` / `StreamProvider(...)` etc. We accept any
  /// type whose name ends in `Provider` so families & auto-dispose
  /// variants are picked up without an enumeration.
  @override
  void visitTopLevelVariableDeclaration(TopLevelVariableDeclaration node) {
    if (currentClassId != null) {
      return; // class-level fields handled elsewhere
    }
    _recordConstStringVariables(node.variables);
    for (final v in node.variables.variables) {
      final init = v.initializer;
      if (init == null) continue;
      if (!_looksLikeProviderConstruction(init)) continue;
      final name = v.name.lexeme;
      final id = 'dart_provider::$rel#$name';
      final lines = _lineRange(node.offset, node.end);
      symbols.add({
        'id': id,
        'kind': NodeKindString.dartProvider,
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
        'symbol_kind': NodeKindString.dartProvider,
        'qualified_name': name,
      });
      // Register the variable element AND its synthetic getter so usages
      // like `ref.read(proProvider)` — which resolve to the getter, not
      // the variable — can be mapped back to the same id.
      final el = v.declaredElement2;
      if (el != null) {
        byElement[el] = id;
        try {
          final getter = (el as dynamic).getter2;
          if (getter is Element2) byElement[getter] = id;
        } catch (_) {
          // analyzer surface still in flux; fall back to name lookup.
        }
      }
      // Always keep a name-based lookup so the body visitor can resolve
      // by `(file, name)` if the analyzer hands us a different element
      // instance for usage sites.
      providerByFileAndName['$rel|$name'] = id;
    }
    super.visitTopLevelVariableDeclaration(node);
  }

  void _recordConstStringVariables(VariableDeclarationList variables) {
    final keyword = variables.keyword?.lexeme;
    if (keyword != 'const' && keyword != 'final') return;
    for (final v in variables.variables) {
      final init = v.initializer;
      if (init is SimpleStringLiteral) {
        constStringByFileAndName['$rel|${v.name.lexeme}'] = init.value;
      }
    }
  }

  bool _looksLikeProviderConstruction(Expression init) {
    // Riverpod providers are always built by invoking a constructor whose
    // class name ends in "Provider" — `Provider`, `StateNotifierProvider`,
    // `ChangeNotifierProvider`, `AutoDisposeFutureProvider`,
    // `StateNotifierProvider.family`, etc. We restrict the textual probe
    // to the "head" of the initializer (everything before the first `(`)
    // so argument identifiers cannot accidentally match.
    final text = init.toSource();
    final paren = text.indexOf('(');
    final head = paren < 0 ? text : text.substring(0, paren);
    final stripped = head.split('<').first; // drop generics
    if (stripped.endsWith('Provider')) return true;
    if (stripped.contains('Provider.')) return true;
    return false;
  }

  ({int start, int end}) _lineRange(int offsetStart, int offsetEnd) {
    final s = lineInfo.getLocation(offsetStart).lineNumber;
    final e = lineInfo.getLocation(offsetEnd).lineNumber;
    return (start: s, end: e);
  }
}

/// Collects `test('name', ...)` and `group('name', ...)` calls as test
/// artifacts. This is intentionally syntax-level: the target repo may use
/// package:test, flutter_test, or local wrappers, and SpecSlice only needs a
/// stable fact that a named test/group exists at a source location.
class _TestDeclarationVisitor extends RecursiveAstVisitor<void> {
  final String rel;
  final dynamic lineInfo;
  final List<Map<String, dynamic>> tests;
  final Set<String> _seenIds = <String>{};

  _TestDeclarationVisitor({
    required this.rel,
    required this.lineInfo,
    required this.tests,
  });

  @override
  void visitMethodInvocation(MethodInvocation node) {
    _tryAdd(node.methodName.name, node.argumentList.arguments, node.offset,
        node.end);
    super.visitMethodInvocation(node);
  }

  @override
  void visitFunctionExpressionInvocation(FunctionExpressionInvocation node) {
    final fn = node.function;
    if (fn is SimpleIdentifier) {
      _tryAdd(fn.name, node.argumentList.arguments, node.offset, node.end);
    }
    super.visitFunctionExpressionInvocation(node);
  }

  void _tryAdd(String callee, NodeList<Expression> args, int offsetStart,
      int offsetEnd) {
    if (callee != 'test' && callee != 'group') return;
    if (args.isEmpty) return;
    final name = _stringLiteralValue(args.first);
    if (name == null || name.trim().isEmpty) return;
    final lines = _lineRange(offsetStart, offsetEnd);
    final slug = _slugify(name);
    final prefix = callee == 'group' ? 'dart_group' : 'dart_test';
    final kind =
        callee == 'group' ? NodeKindString.testGroup : NodeKindString.testCase;
    var id = '$prefix::$rel#$slug';
    if (!_seenIds.add(id)) {
      id = '$prefix::$rel#$slug-line-${lines.start}';
      _seenIds.add(id);
    }
    tests.add({
      'id': id,
      'kind': kind,
      'path': rel,
      'name': name,
      'start_line': lines.start,
      'end_line': lines.end,
      'parent_symbol_id': null,
    });
  }

  String? _stringLiteralValue(AstNode? n) {
    if (n is SimpleStringLiteral) return n.value;
    if (n is AdjacentStrings) {
      final buf = StringBuffer();
      for (final s in n.strings) {
        if (s is SimpleStringLiteral) {
          buf.write(s.value);
        } else {
          return null;
        }
      }
      return buf.toString();
    }
    return null;
  }

  String _slugify(String value) => _slugifyTestName(value);

  ({int start, int end}) _lineRange(int offsetStart, int offsetEnd) {
    final s = lineInfo.getLocation(offsetStart).lineNumber;
    final e = lineInfo.getLocation(offsetEnd).lineNumber;
    return (start: s, end: e);
  }
}

/// Walks method / function bodies and emits `calls` / `references` /
/// P8 framework-aware semantic edges against the symbol table the
/// declaration pass built.
class _BodyVisitor extends RecursiveAstVisitor<void> {
  final String rel;
  final dynamic lineInfo;
  final String source;
  final Map<Element2, String> byElement;
  final Map<String, String> providerByFileAndName;
  final Map<String, String> constStringByFileAndName;
  final List<Map<String, dynamic>> references;
  final Map<String, Map<String, dynamic>> syntheticNodes;

  String? _currentSymbolId;
  final Set<String> _emitted = <String>{};
  // P2 — local-variable Hive box tracking. After `final box =
  // Hive.openBox('pro_entitlement');` we remember `box` → `pro_entitlement`,
  // so any later `box.put/get/delete/clear(...)` site can still emit
  // `persists_to storage::hive::pro_entitlement`. We index by both
  // resolved [Element2] (for accurate scope handling) and by per-method
  // variable name (as a fallback when the analyzer's local-element id
  // doesn't match the SimpleIdentifier's resolved element).
  final Map<Element2, String> _hiveBoxByVariable = <Element2, String>{};
  final Map<String, String> _hiveBoxByName = <String, String>{};

  _BodyVisitor({
    required this.rel,
    required this.lineInfo,
    required this.source,
    required this.byElement,
    required this.providerByFileAndName,
    required this.constStringByFileAndName,
    required this.references,
    required this.syntheticNodes,
  });

  @override
  void visitVariableDeclaration(VariableDeclaration node) {
    // Identify `var foo = Hive.openBox('name')` / `... = Hive.box('name')`,
    // optionally wrapped in `await`. We don't need to know the variable's
    // type — the surface call shape is enough.
    final initializer = node.initializer;
    final boxName = _hiveBoxNameFromExpression(initializer);
    if (boxName != null) {
      final el = node.declaredElement2;
      if (el != null) {
        _hiveBoxByVariable[el] = boxName;
      }
      // Name-based fallback: the analyzer sometimes binds variable
      // *uses* to a different element id than the declaration (esp.
      // around inference / `final` / `await`). The combination of the
      // current method symbol and the variable's lexeme keeps the
      // fallback scoped tightly enough to be safe.
      final name = node.name.lexeme;
      final scope = _currentSymbolId ?? rel;
      _hiveBoxByName['$scope|$name'] = boxName;
    }
    super.visitVariableDeclaration(node);
  }

  @override
  void visitTopLevelVariableDeclaration(TopLevelVariableDeclaration node) {
    // Top-level (file-scope) initializers belong to no symbol — e.g. a
    // registration list `final _scenes = [_Scene(capture: _captureHome), …]`
    // or a tear-off table `const handlers = {0: _onZero};`. With no enclosing
    // class or function, `_currentSymbolId` is null and every reference inside
    // was dropped, so each registered callable looked like high-confidence
    // dead code (dogfood regression: hama integration_test screenshot scenes).
    // Anchor the initializer's references — tear-offs, constructions, calls —
    // on the file node so dead-code reachability keeps the registered targets
    // alive once the file itself is reachable.
    final prev = _currentSymbolId;
    _currentSymbolId = 'file::$rel';
    super.visitTopLevelVariableDeclaration(node);
    _currentSymbolId = prev;
  }

  String? _hiveBoxNameFromExpression(Expression? expr) {
    if (expr == null) return null;
    Expression e = expr;
    if (e is AwaitExpression) e = e.expression;
    if (e is MethodInvocation) {
      final target = e.target;
      if (target is SimpleIdentifier &&
          target.name == 'Hive' &&
          (e.methodName.name == 'box' || e.methodName.name == 'openBox')) {
        final args = e.argumentList.arguments;
        if (args.isNotEmpty) {
          return _stringLiteralValue(args.first);
        }
      }
    }
    return null;
  }

  @override
  void visitClassDeclaration(ClassDeclaration node) {
    // Entering a class scope sets `_currentSymbolId` to the class so that
    // expressions living *directly* at class scope — static / instance
    // field initializers, in particular method tear-offs like
    // `static ... imageExporter = _impl;` — attribute their references to
    // the class instead of being dropped on the floor (dogfood regression:
    // hama's `ExportService` static-field pipeline seams). It also lets us
    // record `extends` / `with` / `implements` as type usages so a
    // superclass / mixin / interface used only structurally stays alive.
    final el = node.declaredFragment?.element;
    final prev = _currentSymbolId;
    _currentSymbolId = el == null ? null : byElement[el];
    _emitTypeUsage(node.extendsClause?.superclass);
    _emitTypeUsages(node.withClause?.mixinTypes);
    _emitTypeUsages(node.implementsClause?.interfaces);
    super.visitClassDeclaration(node);
    _currentSymbolId = prev;
  }

  @override
  void visitMixinDeclaration(MixinDeclaration node) {
    final el = node.declaredFragment?.element;
    final prev = _currentSymbolId;
    _currentSymbolId = el == null ? null : byElement[el];
    _emitTypeUsages(node.onClause?.superclassConstraints);
    _emitTypeUsages(node.implementsClause?.interfaces);
    super.visitMixinDeclaration(node);
    _currentSymbolId = prev;
  }

  @override
  void visitEnumDeclaration(EnumDeclaration node) {
    final el = node.declaredFragment?.element;
    final prev = _currentSymbolId;
    _currentSymbolId = el == null ? null : byElement[el];
    _emitTypeUsages(node.withClause?.mixinTypes);
    _emitTypeUsages(node.implementsClause?.interfaces);
    super.visitEnumDeclaration(node);
    _currentSymbolId = prev;
  }

  void _emitTypeUsages(Iterable<NamedType>? types) {
    if (types == null) return;
    for (final t in types) {
      _emitTypeUsage(t);
    }
  }

  /// Emit a `references` edge from the current symbol to the element a
  /// `NamedType` resolves to (a superclass / mixin / interface). Only
  /// repo-local types that the declaration pass mapped into [byElement]
  /// produce an edge; external SDK / package types are skipped.
  void _emitTypeUsage(NamedType? type) {
    if (type == null) return;
    final from = _currentSymbolId;
    if (from == null) return;
    final el = type.element2;
    if (el == null) return;
    final to = byElement[el];
    if (to == null) return;
    _emit(from, to, EdgeKindString.references, type.offset, type.end);
  }

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
    // P2 — `test('name', () { ... })` / `group('name', () { ... })`:
    // attribute references inside the closure to the synthetic
    // `dart_test::` / `dart_group::` symbol so test files don't drop
    // their references on the floor. We only enter this branch in test
    // sources to avoid mis-attributing arbitrary `test('x', ...)`-shaped
    // calls in production code.
    final methodName = node.methodName.name;
    if ((methodName == 'test' || methodName == 'group') &&
        _isTestSourcePath(rel) &&
        node.argumentList.arguments.length >= 2) {
      final nameArg = node.argumentList.arguments.first;
      final nameLit = _testNameLiteral(nameArg);
      if (nameLit != null) {
        final slug = _slugify(nameLit);
        final prefix = methodName == 'group' ? 'dart_group' : 'dart_test';
        final synthetic = '$prefix::$rel#$slug';
        final prev = _currentSymbolId;
        _currentSymbolId = synthetic;
        // Visit the closure body specifically; skip the literal name
        // and other arguments to avoid mis-attributing them.
        node.argumentList.arguments.skip(1).forEach((arg) {
          arg.accept(this);
        });
        _currentSymbolId = prev;
        return; // do not super-visit; we already walked the args.
      }
    }
    final from = _currentSymbolId;
    if (from != null) {
      // P8 — try framework-aware patterns *first*. They are more specific
      // than a plain `calls` edge and the goal is to surface business
      // semantics (which provider we read, which route we navigate to,
      // which storage we persist to, which stream we subscribed to) over
      // the raw call relationship. We deliberately do not emit BOTH a
      // `calls` and a `reads_provider` for the same site — only the
      // most-specific semantic edge wins.
      if (_tryEmitSemanticEdge(from, node)) {
        super.visitMethodInvocation(node);
        return;
      }
    }
    final target = node.methodName.element;
    if (from != null && target != null) {
      final to = byElement[target];
      if (to != null) {
        _emit(from, to, EdgeKindString.calls, node.offset, node.end);
      }
    }
    super.visitMethodInvocation(node);
  }

  String? _testNameLiteral(Expression e) {
    if (e is SimpleStringLiteral) return e.value;
    if (e is AdjacentStrings) {
      final buf = StringBuffer();
      for (final s in e.strings) {
        if (s is SimpleStringLiteral) {
          buf.write(s.value);
        } else {
          return null;
        }
      }
      return buf.toString();
    }
    return null;
  }

  String _slugify(String s) => _slugifyTestName(s);

  /// Tries to recognise a Flutter / Riverpod / Hive / Stream pattern and
  /// emits the corresponding P8 semantic edge. Returns `true` when a
  /// semantic edge was emitted (so the caller skips the plain `calls`).
  bool _tryEmitSemanticEdge(String from, MethodInvocation node) {
    final methodName = node.methodName.name;
    final target = node.target;
    final targetText = target?.toSource();
    // ---- Riverpod ref.read / ref.watch / ref.listen ----------------------
    if ((methodName == 'read' ||
            methodName == 'watch' ||
            methodName == 'listen') &&
        target != null &&
        _isRefLikeReceiver(targetText)) {
      final providerExpr = node.argumentList.arguments.isNotEmpty
          ? node.argumentList.arguments.first
          : null;
      if (providerExpr != null) {
        final providerId = _resolveProviderId(providerExpr);
        if (providerId != null) {
          _emit(from, providerId, EdgeKindString.readsProvider, node.offset,
              node.end);
          return true;
        }
      }
    }
    // ---- Navigation -----------------------------------------------------
    // context.push / context.go / context.pushNamed / context.pushReplacement
    // Navigator.pushNamed(context, 'name') / Navigator.push(...)
    // GoRouter.of(context).go('/foo')
    final isContextNavigate = target != null &&
        targetText != null &&
        _isContextLikeReceiver(targetText) &&
        const {
          'push',
          'go',
          'pushNamed',
          'goNamed',
          'pushReplacement',
          'pushReplacementNamed',
          'replace',
        }.contains(methodName);
    final isNavigatorStatic = target is SimpleIdentifier &&
        target.name == 'Navigator' &&
        const {'push', 'pushNamed', 'pushReplacementNamed', 'pushReplacement'}
            .contains(methodName);
    if (isContextNavigate || isNavigatorStatic) {
      final route =
          _extractRouteString(node, navigatorStyle: isNavigatorStatic);
      if (route != null) {
        final id = 'route::$route';
        syntheticNodes.putIfAbsent(
            id,
            () => {
                  'id': id,
                  'kind': NodeKindString.route,
                  'label': route,
                });
        _emit(from, id, EdgeKindString.navigatesTo, node.offset, node.end);
        return true;
      }
    }
    // ---- Hive persistence ------------------------------------------------
    // Two shapes:
    //   1) Hive.box('name').put/get/delete/clear(...)
    //   2) someBoxVar.put/get/delete(...)   (after a previous Hive.box call)
    // We capture shape (1) directly because the box name is right there.
    final hiveCall = _matchHiveCall(node, target, methodName);
    if (hiveCall != null) {
      final id = 'storage::hive::${hiveCall.boxName}';
      syntheticNodes.putIfAbsent(
          id,
          () => {
                'id': id,
                'kind': NodeKindString.storage,
                'label': 'hive:${hiveCall.boxName}',
              });
      _emit(from, id, EdgeKindString.persistsTo, node.offset, node.end);
      return true;
    }
    // ---- SharedPreferences ---------------------------------------------
    // prefs.setBool('key', ...) / prefs.setString(...) / prefs.getBool(...)
    if (target != null &&
        _isSharedPrefsReceiver(target) &&
        _isPrefsMethod(methodName)) {
      final keyExpr = node.argumentList.arguments.isNotEmpty
          ? node.argumentList.arguments.first
          : null;
      final keyLit = _stringLiteralValue(keyExpr);
      if (keyLit != null) {
        final id = 'storage::shared_prefs::$keyLit';
        syntheticNodes.putIfAbsent(
            id,
            () => {
                  'id': id,
                  'kind': NodeKindString.storage,
                  'label': 'shared_prefs:$keyLit',
                });
        _emit(from, id, EdgeKindString.persistsTo, node.offset, node.end);
        return true;
      }
    }
    // ---- Stream subscription -------------------------------------------
    // foo.listen(callback) where foo is a Stream<T>.
    if (methodName == 'listen' && target != null) {
      final t = target.staticType;
      if (t != null && _staticTypeIsStream(t)) {
        // Target node = the producer of the stream. If the receiver
        // resolves to a known element in our symbol table, point at it;
        // otherwise synthesise a storage-style node from the receiver
        // expression so the edge is still visible.
        String? producerId;
        if (target is Identifier) {
          final el = target.element;
          if (el != null) producerId = byElement[el];
        }
        if (producerId == null) {
          final label = targetText ?? 'stream';
          producerId = 'storage::stream::$label';
          syntheticNodes.putIfAbsent(
              producerId,
              () => {
                    'id': producerId!,
                    'kind': NodeKindString.storage,
                    'label': 'stream:$label',
                  });
        }
        _emit(from, producerId, EdgeKindString.subscribesStream, node.offset,
            node.end);
        return true;
      }
    }
    return false;
  }

  bool _isRefLikeReceiver(String? text) {
    if (text == null) return false;
    // `ref` (Riverpod Ref / WidgetRef inside a ConsumerWidget), or any
    // identifier ending in `ref` (e.g. `_ref`, `widgetRef`). Restrictive
    // enough to avoid false positives like `instance.read` while still
    // accepting common Riverpod naming.
    if (text == 'ref') return true;
    if (text.endsWith('.ref')) return true;
    return false;
  }

  bool _isContextLikeReceiver(String text) {
    // Accept `context`, `this.context`, `widget.context`, or
    // `GoRouter.of(context)` / `Navigator.of(context)` invocations.
    if (text == 'context') return true;
    if (text.endsWith('.context')) return true;
    if (text.contains('GoRouter.of(') || text.contains('Navigator.of(')) {
      return true;
    }
    return false;
  }

  String? _extractRouteString(MethodInvocation node,
      {required bool navigatorStyle}) {
    final args = node.argumentList.arguments;
    if (args.isEmpty) return null;
    // Navigator.pushNamed(context, '/foo') — the route string is arg[1].
    final routeArg = navigatorStyle && args.length >= 2 ? args[1] : args.first;
    return _stringLiteralValue(routeArg);
  }

  String? _stringLiteralValue(AstNode? n) {
    if (n is SimpleStringLiteral) return n.value;
    if (n is SimpleIdentifier) {
      return constStringByFileAndName['$rel|${n.name}'];
    }
    if (n is AdjacentStrings) {
      // Concat all parts if they are all simple literals.
      final buf = StringBuffer();
      for (final s in n.strings) {
        if (s is SimpleStringLiteral) {
          buf.write(s.value);
        } else {
          return null;
        }
      }
      return buf.toString();
    }
    return null;
  }

  _HiveCall? _matchHiveCall(
      MethodInvocation node, Expression? target, String methodName) {
    // Recognise `Hive.openBox('name')` / `Hive.box('name')` itself. This
    // matters for code that stores the box in a variable before calling
    // `put` or `get`, because the storage boundary is still visible at
    // the open site.
    if (target is SimpleIdentifier &&
        target.name == 'Hive' &&
        (methodName == 'box' || methodName == 'openBox')) {
      final boxNameArg = node.argumentList.arguments.isNotEmpty
          ? node.argumentList.arguments.first
          : null;
      final boxName = _stringLiteralValue(boxNameArg);
      if (boxName != null) {
        return _HiveCall(boxName: boxName);
      }
    }
    // Recognise `Hive.box('name').put/get/delete(...)` — the receiver of
    // our MethodInvocation is itself a MethodInvocation of `Hive.box`.
    if (target is MethodInvocation) {
      final inner = target;
      final innerReceiver = inner.target;
      if (innerReceiver is SimpleIdentifier &&
          innerReceiver.name == 'Hive' &&
          (inner.methodName.name == 'box' ||
              inner.methodName.name == 'openBox')) {
        final boxNameArg = inner.argumentList.arguments.isNotEmpty
            ? inner.argumentList.arguments.first
            : null;
        final boxName = _stringLiteralValue(boxNameArg);
        if (boxName != null && _isHiveOperation(methodName)) {
          return _HiveCall(boxName: boxName);
        }
      }
    }
    // P2 — local-variable Hive box tracking. `final box = Hive.box('x');
    // box.put('k', v);` — the second site has a SimpleIdentifier
    // receiver that resolves to a variable we previously tagged with a
    // box name in [visitVariableDeclaration].
    if (target is SimpleIdentifier && _isHiveOperation(methodName)) {
      final el = target.element;
      if (el != null) {
        final boxName = _hiveBoxByVariable[el];
        if (boxName != null) {
          return _HiveCall(boxName: boxName);
        }
      }
      // Fallback: name-based lookup scoped to the current method.
      final scope = _currentSymbolId ?? rel;
      final byName = _hiveBoxByName['$scope|${target.name}'];
      if (byName != null) {
        return _HiveCall(boxName: byName);
      }
    }
    return null;
  }

  bool _isHiveOperation(String methodName) {
    return const {
      'put',
      'putAll',
      'get',
      'delete',
      'deleteAll',
      'clear',
      'add',
      'addAll',
      'putAt',
      'getAt',
    }.contains(methodName);
  }

  bool _isSharedPrefsReceiver(Expression target) {
    final t = target.staticType;
    if (t != null) {
      final name = t.element3?.name3 ?? '';
      if (name == 'SharedPreferences') return true;
    }
    // Fallback to textual probe — naming convention is universal.
    final src = target.toSource();
    return src == 'prefs' || src == '_prefs' || src.endsWith('.prefs');
  }

  bool _isPrefsMethod(String name) {
    return name.startsWith('set') ||
        name.startsWith('get') ||
        name == 'remove' ||
        name == 'clear';
  }

  bool _staticTypeIsStream(dynamic t) {
    // `t` is a `DartType`. The simplest correct probe is to follow its
    // supertypes looking for `Stream`. We use the textual rendering of
    // the type as a safe fallback if the analyzer surface shifts.
    try {
      final name = t.element3?.name3 ?? '';
      if (name == 'Stream') return true;
    } catch (_) {
      // Fall through.
    }
    final disp = t.toString();
    return disp.startsWith('Stream<') || disp == 'Stream';
  }

  String? _resolveProviderId(Expression providerExpr) {
    // `proProvider` → dart_provider::file#proProvider when that variable
    // is in our symbol table. `proProvider.notifier` collapses to the
    // base provider. `myFamily('id')` (Riverpod family) collapses to the
    // family's base provider symbol.
    Expression base = providerExpr;
    if (base is PrefixedIdentifier) {
      base = base.prefix;
    } else if (base is PropertyAccess) {
      base = base.target ?? base;
    } else if (base is MethodInvocation) {
      final t = base.target;
      if (t != null) base = t;
    }
    String? identifierName;
    if (base is SimpleIdentifier) {
      identifierName = base.name;
      // First try the element-direct map (most accurate).
      final el = base.element;
      if (el != null) {
        final mapped = byElement[el];
        if (mapped != null && mapped.startsWith('dart_provider::')) {
          return mapped;
        }
      }
    } else if (base is PrefixedIdentifier) {
      identifierName = base.prefix.name;
    } else if (base is Identifier) {
      identifierName = base.toSource();
      final el = base.element;
      if (el != null) {
        final mapped = byElement[el];
        if (mapped != null && mapped.startsWith('dart_provider::')) {
          return mapped;
        }
      }
    }
    // Element-direct lookup missed (e.g. usage site resolves to a
    // synthetic getter, not the variable). Fall back to file+name —
    // imports are repo-relative so we can scan every registered
    // provider name in any file and pick the first match.
    if (identifierName != null) {
      // 1. Same file first.
      final sameFile = providerByFileAndName['$rel|$identifierName'];
      if (sameFile != null) return sameFile;
      // 2. Any file. Identical names across files are rare for Riverpod
      // providers (they tend to be globally unique by convention).
      for (final entry in providerByFileAndName.entries) {
        if (entry.key.endsWith('|$identifierName')) return entry.value;
      }
    }
    return null;
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
        final classId = byElement[enclosing];
        if (classId != null) {
          _emit(
              from, classId, EdgeKindString.references, node.offset, node.end);
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

class _HiveCall {
  final String boxName;
  _HiveCall({required this.boxName});
}
