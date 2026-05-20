import '../../shims/flutter_shims.dart';

class Layer {
  final String id;
  String name;
  bool visible;
  double opacity;

  Layer({
    required this.id,
    required this.name,
    this.visible = true,
    this.opacity = 1.0,
  });
}

/// Layer repository — owns a typed list of layers and persists each
/// mutation to a single Hive box (`project_layers`). Walker assertions:
///   * `persists_to storage::hive::project_layers` from every mutator
///   * `calls`/`references` graph linking the controller to the repo
class LayerRepository {
  final List<Layer> _layers = <Layer>[];

  List<Layer> get layers => List<Layer>.unmodifiable(_layers);

  Future<void> addLayer(Layer layer) async {
    _layers.add(layer);
    await _persist();
  }

  Future<void> removeLayer(String id) async {
    _layers.removeWhere((l) => l.id == id);
    await _persist();
  }

  Future<void> reorder(int oldIndex, int newIndex) async {
    if (oldIndex < 0 ||
        newIndex < 0 ||
        oldIndex >= _layers.length ||
        newIndex >= _layers.length) {
      return;
    }
    final moved = _layers.removeAt(oldIndex);
    _layers.insert(newIndex, moved);
    await _persist();
  }

  Future<void> setVisibility(String id, bool visible) async {
    for (final layer in _layers) {
      if (layer.id == id) {
        layer.visible = visible;
        await _persist();
        return;
      }
    }
  }

  Future<void> _persist() async {
    final box = await Hive.openBox('project_layers');
    box.put('count', _layers.length);
  }
}

class LayerController {
  final LayerRepository repository;

  LayerController(this.repository);

  Future<void> appendBlank(String id, String name) async {
    await repository.addLayer(Layer(id: id, name: name));
  }

  Future<void> deleteLayer(String id) async {
    await repository.removeLayer(id);
  }
}

final layerRepositoryProvider = Provider<LayerRepository>(
  (ref) => LayerRepository(),
);
