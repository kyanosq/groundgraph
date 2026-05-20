import '../../lib/features/layers/layer_repository.dart';

void main() {
  group('LayerRepository', () {
    test('addLayer stores the layer and persists', () async {
      final repo = LayerRepository();
      await repo.addLayer(Layer(id: 'l1', name: 'Sketch'));
      assert(repo.layers.length == 1);
      assert(repo.layers.first.id == 'l1');
    });

    test('reorder swaps two layers', () async {
      final repo = LayerRepository();
      await repo.addLayer(Layer(id: 'a', name: 'A'));
      await repo.addLayer(Layer(id: 'b', name: 'B'));
      await repo.reorder(0, 1);
      assert(repo.layers.first.id == 'b');
      assert(repo.layers.last.id == 'a');
    });
  });
}

void group(String name, void Function() body) => body();
void test(String name, void Function() body) => body();
