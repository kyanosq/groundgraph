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
}
