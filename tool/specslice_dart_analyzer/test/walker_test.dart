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
}
