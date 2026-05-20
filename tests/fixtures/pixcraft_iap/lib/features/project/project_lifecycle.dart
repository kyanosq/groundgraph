import '../../shims/flutter_shims.dart';
import '../layers/layer_repository.dart';

class Project {
  final String id;
  String name;
  DateTime? lastOpenedAt;

  Project({required this.id, required this.name, this.lastOpenedAt});
}

class ProjectEvent {
  final String type;
  final String projectId;
  ProjectEvent(this.type, this.projectId);
}

class ProjectEventBus {
  Stream<ProjectEvent> get events =>
      Stream<ProjectEvent>.fromIterable(const <ProjectEvent>[]);
}

/// Project lifecycle controller — create / load / save / close. Each
/// mutator persists to Hive via a captured `box` local variable; the
/// controller also subscribes to a `ProjectEventBus.events` stream to
/// react to autosave events from other parts of the editor.
///
/// Expected P2/P8 edges:
///   * `persists_to storage::hive::projects`   (create / save / close)
///   * `subscribes_stream` on `ProjectEventBus.events`
///   * `navigates_to route::/projects`         (closeProject)
///   * `calls` LayerRepository methods         (loadProject)
class ProjectLifecycle {
  Project? currentProject;
  final LayerRepository layerRepository;
  final ProjectEventBus eventBus;

  ProjectLifecycle({
    required this.layerRepository,
    required this.eventBus,
  });

  Future<void> createProject(String id, String name) async {
    final box = await Hive.openBox('projects');
    box.put(id, name);
    currentProject = Project(
      id: id,
      name: name,
      lastOpenedAt: DateTime.now(),
    );
  }

  Future<void> loadProject(String id) async {
    final box = await Hive.openBox('projects');
    final name = box.get(id);
    if (name is String) {
      currentProject = Project(
        id: id,
        name: name,
        lastOpenedAt: DateTime.now(),
      );
      // Refresh layers so the editor opens with the project's content.
      await layerRepository.addLayer(Layer(id: 'background', name: 'Background'));
    }
  }

  Future<void> saveProject() async {
    final project = currentProject;
    if (project == null) return;
    final box = await Hive.openBox('projects');
    box.put(project.id, project.name);
  }

  Future<void> closeProject(BuildContext context) async {
    final project = currentProject;
    if (project != null) {
      final box = await Hive.openBox('projects');
      box.put('lastClosed', project.id);
    }
    currentProject = null;
    context.go('/projects');
  }

  void subscribeAutosave() {
    eventBus.events.listen((ProjectEvent event) {
      // Fire-and-forget autosave; ignores result on purpose for the
      // fixture but the candidate flags the missing cancellation.
      saveProject();
    });
  }
}

final projectLifecycleProvider =
    Provider<ProjectLifecycle>((ref) => ProjectLifecycle(
          layerRepository: LayerRepository(),
          eventBus: ProjectEventBus(),
        ));
