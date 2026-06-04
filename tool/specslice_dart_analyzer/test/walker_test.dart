import 'dart:io';

import 'package:path/path.dart' as p;
import 'package:specslice_dart_analyzer/protocol.dart';
import 'package:specslice_dart_analyzer/walker.dart';
import 'package:test/test.dart';

void main() {
  test('walkRepository emits test and group artifacts', () async {
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));

    final testDir = Directory(p.join(root.path, 'test', 'iap'));
    testDir.createSync(recursive: true);
    File(p.join(testDir.path, 'iap_constants_test.dart')).writeAsStringSync('''
void main() {
  group('iap constants', () {
    test('exposes monthly/yearly/lifetime ids', () {});
  });
}

void group(String name, void Function() body) => body();
void test(String name, void Function() body) => body();
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['test']),
    );
    final tests = response.toJson()['tests'] as List<dynamic>? ?? const [];
    expect(
      tests.map((e) => e['kind']),
      containsAll([NodeKindString.testGroup, NodeKindString.testCase]),
    );
    expect(
      tests.map((e) => e['id']),
      containsAll([
        'dart_group::test/iap/iap_constants_test.dart#iap-constants',
        'dart_test::test/iap/iap_constants_test.dart#exposes-monthly-yearly-lifetime-ids',
      ]),
    );
  });

  test('walkRepository ignores test-shaped calls outside test sources',
      () async {
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));

    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'diagnostics.dart')).writeAsStringSync('''
void runDiagnostics() {
  test('not a package test', () {});
}

void test(String name, void Function() body) => body();
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final tests = response.toJson()['tests'] as List<dynamic>? ?? const [];
    expect(tests, isEmpty);
  });

  test(
      'walkRepository emits persists_to for box-variable usage in a method where openBox happens upstream',
      () async {
    // 业务代码常见写法：openBox 在 init 路径里，业务方法只看到 box 变量
    // 然后调用 box.put / box.get。我们希望 sidecar 在不直接看到 openBox
    // 调用的方法里，仍然能基于局部变量追踪发出 persists_to。
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'pro_provider.dart')).writeAsStringSync('''
class ProNotifier {
  Future<void> applyPurchase() async {
    final box = await Hive.openBox('pro_entitlement');
    await box.put('isPro', true);
    final value = await box.get('isPro');
    print(value);
  }
}

class Hive {
  static Future<Box> openBox(String name) async => Box();
}

class Box {
  Future<void> put(String key, Object value) async {}
  Future<Object?> get(String key) async => null;
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        response.toJson()['references'] as List<dynamic>? ?? const [];
    final persistsRefs = references
        .whereType<Map<String, dynamic>>()
        .where((e) =>
            e['kind'] == EdgeKindString.persistsTo &&
            e['to_symbol_id'] == 'storage::hive::pro_entitlement')
        .toList();
    // The walker dedups by (from, to, kind) so we only expect one
    // persists_to edge per call site; what we *do* expect is that the
    // first emitter site is the local-variable-aware one — the openBox
    // line OR the box.put line, both attributing the edge to
    // ProNotifier.applyPurchase.
    expect(
      persistsRefs,
      isNotEmpty,
      reason: '应至少发出一条 persists_to。当前 references: $references',
    );
    expect(
      persistsRefs.first['from_symbol_id'],
      'dart_method::lib/pro_provider.dart#ProNotifier.applyPurchase',
    );
  });

  test(
      'walkRepository emits persists_to from a box-variable put in a method where openBox is not in source',
      () async {
    // Truly exercise the local-variable fallback: the method that calls
    // `box.put(...)` does NOT itself call openBox. We still want
    // persists_to from that method to the storage target.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'pro_provider.dart')).writeAsStringSync('''
class ProNotifier {
  void doPersist() {
    final box = Hive.box('pro_entitlement');
    box.put('isPro', true);
    box.put('isPro2', false);
  }
}

class Hive {
  static Box box(String name) => Box();
}

class Box {
  void put(String key, Object value) {}
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        response.toJson()['references'] as List<dynamic>? ?? const [];
    final persistsRefs = references
        .whereType<Map<String, dynamic>>()
        .where((e) =>
            e['kind'] == EdgeKindString.persistsTo &&
            e['to_symbol_id'] == 'storage::hive::pro_entitlement' &&
            e['from_symbol_id'] ==
                'dart_method::lib/pro_provider.dart#ProNotifier.doPersist')
        .toList();
    expect(
      persistsRefs,
      isNotEmpty,
      reason: 'local-variable Hive 追踪未触发：$references',
    );
  });

  test(
      'walkRepository emits calls edge through ref.read(provider.notifier).method(...)',
      () async {
    // P2 — 业务里最常见的 Riverpod 触发链：从 widget 通过 ref.read 拿到
    // notifier 再调用其方法。我们希望 sidecar 在 ref.read 处发出
    // reads_provider，并在外层 .applyPurchase(...) 调用处发出 calls 到
    // 真实的 dart_method 符号。
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'pro_provider.dart')).writeAsStringSync('''
class ProNotifier {
  bool state = false;
  void applyPurchase(String id) {
    state = true;
  }
}

class _ProviderForNotifier<T> {
  T get notifier => throw UnimplementedError();
}

final proProvider = _ProviderForNotifier<ProNotifier>();
''');
    File(p.join(libDir.path, 'paywall.dart')).writeAsStringSync('''
import 'pro_provider.dart';

class _Ref {
  T read<T>(T Function() factory) => factory();
}

class Paywall {
  final _Ref ref = _Ref();
  void onUpdate(String id) {
    ref.read<ProNotifier>(() => proProvider.notifier).applyPurchase(id);
  }
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        response.toJson()['references'] as List<dynamic>? ?? const [];
    final calls = references
        .whereType<Map<String, dynamic>>()
        .where((e) => e['kind'] == EdgeKindString.calls)
        .toList();
    expect(
      calls,
      contains(
        isA<Map<String, dynamic>>()
            .having(
              (e) => e['from_symbol_id'],
              'from_symbol_id',
              'dart_method::lib/paywall.dart#Paywall.onUpdate',
            )
            .having(
              (e) => e['to_symbol_id'],
              'to_symbol_id',
              'dart_method::lib/pro_provider.dart#ProNotifier.applyPurchase',
            ),
      ),
      reason:
          '应该能把 ref.read(provider.notifier).applyPurchase(...) 解析到真实 method 符号。'
          '当前 calls: $calls',
    );
  });

  test(
      'walkRepository attributes calls inside a nested local function to the enclosing method',
      () async {
    // Flutter builders routinely construct widgets inside a *named local
    // function* (`Widget _buildPage() { … return Foo(...); }`). The local
    // function has no graph symbol, so the scope must fall back to the
    // enclosing method — otherwise the construction edge is dropped and the
    // constructed widget's ctor looks like dead code (Shift regression:
    // ExportDayCell / ScheduleMonthExportPage built inside a local function).
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'widgets.dart')).writeAsStringSync('''
class Cell {
  const Cell();
}

class Builder {
  Cell build() {
    Cell make() {
      return Cell();
    }
    return make();
  }
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        response.toJson()['references'] as List<dynamic>? ?? const [];
    final calls = references
        .whereType<Map<String, dynamic>>()
        .where((e) => e['kind'] == EdgeKindString.calls)
        .toList();
    expect(
      calls,
      contains(
        isA<Map<String, dynamic>>()
            .having(
              (e) => e['from_symbol_id'],
              'from_symbol_id',
              'dart_method::lib/widgets.dart#Builder.build',
            )
            .having(
              (e) => e['to_symbol_id'],
              'to_symbol_id',
              'dart_ctor::lib/widgets.dart#Cell.<default>',
            ),
      ),
      reason:
          '嵌套局部函数里的构造调用应归属到外层方法 Builder.build。当前 calls: $calls',
    );
  });

  test('walkRepository emits Hive openBox storage edge from const box name',
      () async {
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));

    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'pro_provider.dart')).writeAsStringSync('''
class ProNotifier {
  static const String _boxName = 'settings';

  Future<void> applyPurchase() async {
    final box = await Hive.openBox(_boxName);
    await box.put('pro_entitlement_type_v2', 'lifetime');
  }
}

class Hive {
  static Future<Box> openBox(String name) async => Box();
}

class Box {
  Future<void> put(String key, Object value) async {}
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        response.toJson()['references'] as List<dynamic>? ?? const [];

    expect(
      references,
      contains(
        isA<Map<String, dynamic>>()
            .having((e) => e['kind'], 'kind', EdgeKindString.persistsTo)
            .having(
              (e) => e['from_symbol_id'],
              'from_symbol_id',
              'dart_method::lib/pro_provider.dart#ProNotifier.applyPurchase',
            )
            .having(
              (e) => e['to_symbol_id'],
              'to_symbol_id',
              'storage::hive::settings',
            ),
      ),
    );
  });

  test(
      'walkRepository resolves calls/references to extension members used via implicit this',
      () async {
    // Regression (dogfood, tailorx): big widgets are split into `part`
    // files where private helpers live in `extension _X on _State`
    // blocks and are used from sibling extensions via implicit `this`.
    // The declaration pass used to ignore ExtensionDeclaration, so those
    // members were never mapped (Element → id) and *every* private
    // extension member showed up as high-confidence dead code despite
    // being live. tree-sitter ids them under the `on` type
    // (`dart_method::<file>#<OnType>.<member>`); the sidecar must agree
    // so the usage edges actually connect.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'panel.dart')).writeAsStringSync('''
class _PanelState {
  int run() => _useBoth();
}

extension _Fields on _PanelState {
  int _helper() => 1;
  int get _value => 2;
}

extension _Right on _PanelState {
  int _useBoth() => _helper() + _value;
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        (response.toJson()['references'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    const useBoth = 'dart_method::lib/panel.dart#_PanelState._useBoth';
    const helper = 'dart_method::lib/panel.dart#_PanelState._helper';
    const value = 'dart_method::lib/panel.dart#_PanelState._value';
    const run = 'dart_method::lib/panel.dart#_PanelState.run';

    // Cross-extension method call via implicit `this`.
    expect(
      hasEdge(useBoth, helper, EdgeKindString.calls),
      isTrue,
      reason: '_useBoth → _helper (calls) missing: $references',
    );
    // Cross-extension getter read via implicit `this`.
    expect(
      hasEdge(useBoth, value, EdgeKindString.references),
      isTrue,
      reason: '_useBoth → _value (references) missing: $references',
    );
    // Class method into an extension member keeps the extension reachable.
    expect(
      hasEdge(run, useBoth, EdgeKindString.calls),
      isTrue,
      reason: 'run → _useBoth (calls) missing: $references',
    );

    // The sidecar must also EMIT extension members as `dart_method` symbol
    // rows. When tree-sitter mis-parses a file (Dart 3 syntax → ERROR-node
    // cascade), the extension member degrades into a phantom top-level
    // `dart_fn`; the engine can only reconcile that phantom away / backfill
    // the real `dart_method` node when the analyzer overlay actually carries
    // it. Emitting it here is harmless when tree-sitter parses cleanly (the
    // engine drops the duplicate by id). Regression: turing's
    // `game_screen_editor.dart`.
    final symbolIds =
        (response.toJson()['symbols'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .map((s) => s['id'])
            .toSet();
    for (final id in [useBoth, helper, value]) {
      expect(
        symbolIds.contains(id),
        isTrue,
        reason: 'extension member symbol must be emitted: $id (got $symbolIds)',
      );
    }
  });

  test('walkRepository emits a references edge for a mixin applied via with',
      () async {
    // Regression (dogfood, hama): a private mixin used only through a
    // class's `with` clause was flagged high-confidence dead code because
    // the walker never recorded the application as a usage. The `with`
    // clause is a reference to the mixin type.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'x.dart')).writeAsStringSync('''
mixin _ClipboardMixin {
  void copy() {}
}

class Editor with _ClipboardMixin {
  void run() {
    copy();
  }
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        (response.toJson()['references'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      hasEdge('dart_class::lib/x.dart#Editor',
          'dart_class::lib/x.dart#_ClipboardMixin', EdgeKindString.references),
      isTrue,
      reason: 'Editor with _ClipboardMixin must reference the mixin: '
          '$references',
    );
  });

  test('walkRepository emits references edges for extends and implements',
      () async {
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'x.dart')).writeAsStringSync('''
abstract class _Base {
  void m();
}

abstract class _Capable {
  void cap();
}

class Impl extends _Base implements _Capable {
  @override
  void m() {}
  @override
  void cap() {}
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        (response.toJson()['references'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      hasEdge('dart_class::lib/x.dart#Impl', 'dart_class::lib/x.dart#_Base',
          EdgeKindString.references),
      isTrue,
      reason: 'Impl extends _Base must reference _Base: $references',
    );
    expect(
      hasEdge('dart_class::lib/x.dart#Impl', 'dart_class::lib/x.dart#_Capable',
          EdgeKindString.references),
      isTrue,
      reason: 'Impl implements _Capable must reference _Capable: $references',
    );
  });

  test(
      'walkRepository emits a references edge for a static-field tear-off '
      'initializer', () async {
    // Regression (dogfood, hama): `static ... imageExporter = _impl;`
    // assigns a method tear-off in a *static field initializer*, which
    // lives at class scope (no enclosing method body). The body visitor
    // dropped it because `_currentSymbolId` was null at class scope, so
    // `_impl` looked like dead code. The initializer reference must be
    // attributed to the enclosing class.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'x.dart')).writeAsStringSync('''
class Service {
  static int Function() exporter = _impl;

  static int _impl() => 1;
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        (response.toJson()['references'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      hasEdge('dart_class::lib/x.dart#Service',
          'dart_method::lib/x.dart#Service._impl', EdgeKindString.references),
      isTrue,
      reason: 'static-field tear-off must reference Service._impl: '
          '$references',
    );
  });

  test(
      'walkRepository emits a file-anchored references edge for a '
      'top-level variable tear-off initializer', () async {
    // Regression (dogfood, hama integration_test/screenshot_test.dart): a
    // top-level registration list `final _scenes = [_Scene(capture:
    // _captureHome), ...]` references its handlers as tear-offs. These live
    // at *file scope* (no enclosing class or function), so the body visitor
    // dropped them and every `_capture*` looked like high-confidence dead
    // code. The reference must be anchored on the file node so reachability
    // keeps the registered targets alive once the file is reachable.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'x.dart')).writeAsStringSync('''
typedef Capture = void Function();

void _captureHome() {}

final List<Capture> scenes = <Capture>[_captureHome];
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final references =
        (response.toJson()['references'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      hasEdge('file::lib/x.dart', 'dart_fn::lib/x.dart#_captureHome',
          EdgeKindString.references),
      isTrue,
      reason: 'top-level tear-off must reference _captureHome from the file '
          'node: $references',
    );
  });

  test(
      'walkRepository emits constructor edges on the canonical dart_ctor id '
      'scheme so they bind to the tree-sitter structure', () async {
    // Regression (dogfood, hama): the sidecar emitted constructor symbols and
    // construction edges under `dart_constructor::path#Class._default`, but the
    // tree-sitter structural node (and `dart_constructor_id`) use
    // `dart_ctor::path#Class.<default>`. The two never matched, so *every*
    // construction edge dangled (286 in hama) and freshly-constructed classes
    // looked dead. The overlay must address constructors with the canonical id.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'x.dart')).writeAsStringSync('''
class Greeter {
  Greeter();

  String hi() => 'hi';
}

void run() {
  Greeter();
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final json = response.toJson();
    final references = (json['references'] as List<dynamic>? ?? const [])
        .whereType<Map<String, dynamic>>()
        .toList();
    final symbolIds = (json['symbols'] as List<dynamic>? ?? const [])
        .whereType<Map<String, dynamic>>()
        .map((s) => s['id'] as String?)
        .whereType<String>()
        .toSet();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      symbolIds.contains('dart_ctor::lib/x.dart#Greeter.<default>'),
      isTrue,
      reason: 'constructor symbol must use the canonical dart_ctor id: '
          '$symbolIds',
    );
    expect(
      hasEdge('dart_fn::lib/x.dart#run', 'dart_ctor::lib/x.dart#Greeter.<default>',
          EdgeKindString.calls),
      isTrue,
      reason: 'construction must emit a calls edge to the canonical ctor id: '
          '$references',
    );
  });

  test(
      'walkRepository emits a construction edge from a static const field '
      'initializer (l10n delegate shape)', () async {
    // Regression (dogfood, hama l10n): the generated
    // `static const LocalizationsDelegate<…> delegate = _AppLocalizationsDelegate();`
    // is the *only* reference to the private delegate class + its const ctor.
    // The construction lives in a `static const` field initializer at class
    // scope, so the edge must attribute to the enclosing class and target the
    // canonical ctor id; otherwise the delegate class looked like high-
    // confidence dead code.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    final libDir = Directory(p.join(root.path, 'lib'));
    libDir.createSync(recursive: true);
    File(p.join(libDir.path, 'x.dart')).writeAsStringSync('''
abstract class AppLocalizations {
  static const Delegate delegate = _AppLocalizationsDelegate();
}

abstract class Delegate {
  const Delegate();
}

class _AppLocalizationsDelegate extends Delegate {
  const _AppLocalizationsDelegate();
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final json = response.toJson();
    final references = (json['references'] as List<dynamic>? ?? const [])
        .whereType<Map<String, dynamic>>()
        .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      hasEdge(
          'dart_class::lib/x.dart#AppLocalizations',
          'dart_ctor::lib/x.dart#_AppLocalizationsDelegate.<default>',
          EdgeKindString.calls),
      isTrue,
      reason: 'static const field construction must emit a calls edge from the '
          'enclosing class to the canonical ctor id: $references',
    );
  });

  test(
      'walkRepository analyzes files that analysis_options.yaml excludes '
      '(graph coverage must not depend on lint scope)', () async {
    // Regression (dogfood, hama): the project excludes generated localisation
    // files from the *linter* via
    //   analyzer:
    //     exclude:
    //       - lib/l10n/generated/**
    // The tree-sitter structural pass still indexes them (they are real code
    // that runs), but the analyzer sidecar enumerated `analyzedFiles()`, which
    // honours that exclude — so the generated classes got structural nodes
    // with *zero* semantic inbound edges and looked like high-confidence dead
    // code. Graph coverage must follow the code roots, not the project's
    // lint scope. SpecSlice has its own exclude config for graph scoping.
    final root = await Directory.systemTemp.createTemp('specslice_sidecar_');
    addTearDown(() => root.delete(recursive: true));
    File(p.join(root.path, 'analysis_options.yaml')).writeAsStringSync('''
analyzer:
  exclude:
    - lib/generated/**
''');
    final genDir = Directory(p.join(root.path, 'lib', 'generated'));
    genDir.createSync(recursive: true);
    // An excluded file that *constructs* a private class: the only reference
    // to `_Delegate` lives here, so dropping this file strands `_Delegate`.
    File(p.join(genDir.path, 'messages.dart')).writeAsStringSync('''
abstract class Messages {
  static const Object delegate = _Delegate();
}

class _Delegate {
  const _Delegate();
}
''');

    final response = await walkRepository(
      SidecarRequest(repoRoot: root.path, codeRoots: const ['lib']),
    );
    final json = response.toJson();
    final symbolIds = (json['symbols'] as List<dynamic>? ?? const [])
        .whereType<Map<String, dynamic>>()
        .map((s) => s['id'] as String?)
        .whereType<String>()
        .toSet();
    final references = (json['references'] as List<dynamic>? ?? const [])
        .whereType<Map<String, dynamic>>()
        .toList();
    bool hasEdge(String from, String to, String kind) => references.any((e) =>
        e['from_symbol_id'] == from &&
        e['to_symbol_id'] == to &&
        e['kind'] == kind);

    expect(
      symbolIds.any((id) => id.contains('lib/generated/messages.dart')),
      isTrue,
      reason: 'analyzer must emit symbols for lint-excluded files: $symbolIds',
    );
    expect(
      hasEdge(
          'dart_class::lib/generated/messages.dart#Messages',
          'dart_ctor::lib/generated/messages.dart#_Delegate.<default>',
          EdgeKindString.calls),
      isTrue,
      reason: 'construction inside a lint-excluded file must still produce a '
          'semantic edge: $references',
    );
  });

  group('resolveSdkPath', () {
    test('isValidSdk is false for a non-SDK directory', () {
      expect(isValidSdk('/definitely/not/an/sdk'), isFalse);
      expect(isValidSdk(''), isFalse);
    });

    test('returns null under `dart run` (analyzer default already valid)', () {
      // The test harness runs under the real Dart VM, so the analyzer's own
      // `dirname(dirname(resolvedExecutable))` default points at a valid SDK
      // and we must NOT override it.
      expect(resolveSdkPath(), isNull);
    });

    test('any non-null result is a usable SDK root', () {
      final s = resolveSdkPath();
      if (s != null) {
        expect(isValidSdk(s), isTrue, reason: 'resolved $s is not a valid SDK');
      }
    });

    test('recovers a valid SDK via PATH when the executable path is bogus', () {
      // Simulates the AOT-compiled-binary deployment: `resolvedExecutable` is
      // the binary itself (here a throwaway /tmp path), so the analyzer
      // default is bogus and we must fall back to `dart` on PATH. The test
      // host has `dart` on PATH, so a valid SDK must be recovered.
      final recovered = resolveSdkPath(
        resolvedExecutable: '/tmp/specslice_bogus_sidecar_exe',
        environment: Platform.environment,
      );
      expect(recovered, isNotNull,
          reason: 'must locate the SDK via `dart` on PATH');
      expect(isValidSdk(recovered!), isTrue);
    });

    test('prefers SPECSLICE_DART_SDK override when the default is bogus', () {
      final realSdk = resolveSdkPath(
        resolvedExecutable: '/tmp/specslice_bogus_sidecar_exe',
        environment: Platform.environment,
      );
      // Only meaningful when we could find a real SDK to point the override at.
      if (realSdk == null) return;
      final picked = resolveSdkPath(
        resolvedExecutable: '/tmp/specslice_bogus_sidecar_exe',
        environment: {'SPECSLICE_DART_SDK': realSdk, 'PATH': ''},
      );
      expect(picked, realSdk);
    });
  });
}
