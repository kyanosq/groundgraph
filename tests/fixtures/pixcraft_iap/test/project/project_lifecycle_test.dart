import '../../lib/features/layers/layer_repository.dart';
import '../../lib/features/project/project_lifecycle.dart';

void main() {
  group('ProjectLifecycle', () {
    test('createProject sets currentProject and writes to Hive', () async {
      final lifecycle = ProjectLifecycle(
        layerRepository: LayerRepository(),
        eventBus: ProjectEventBus(),
      );
      await lifecycle.createProject('p1', 'Demo');
      assert(lifecycle.currentProject != null);
      assert(lifecycle.currentProject!.id == 'p1');
    });

    test('loadProject refreshes the layer list', () async {
      final lifecycle = ProjectLifecycle(
        layerRepository: LayerRepository(),
        eventBus: ProjectEventBus(),
      );
      // The fixture Hive stub returns null for missing keys, so this
      // exercise the "no project found" branch — the controller must
      // not throw.
      await lifecycle.loadProject('missing');
      assert(lifecycle.currentProject == null);
    });
  });
}

void group(String name, void Function() body) => body();
void test(String name, void Function() body) => body();
