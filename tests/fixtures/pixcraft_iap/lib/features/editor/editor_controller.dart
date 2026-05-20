import '../../shims/flutter_shims.dart';

enum EditorTool { brush, eraser, fill, picker }

class EditorState {
  EditorTool tool;
  int brushSize;
  bool dirty;

  EditorState({
    this.tool = EditorTool.brush,
    this.brushSize = 8,
    this.dirty = false,
  });

  EditorState copyWith({EditorTool? tool, int? brushSize, bool? dirty}) {
    return EditorState(
      tool: tool ?? this.tool,
      brushSize: brushSize ?? this.brushSize,
      dirty: dirty ?? this.dirty,
    );
  }
}

/// Editor controller — coordinates tool selection, undo/redo and
/// persistence of the user's last-used tool to a Hive box. The walker
/// should pick up:
///   * `persists_to storage::hive::editor_state`  (via `Hive.openBox`
///     stored in a local variable, then `box.put(...)`)
///   * `calls dart_method::...#EditorController.applyTool`        from
///     `selectTool`
///   * `navigates_to route::/editor`                              from
///     `openEditor`
class EditorController {
  EditorState state = EditorState();
  final List<EditorState> _undoStack = <EditorState>[];

  Future<void> applyTool(EditorTool tool) async {
    _undoStack.add(state);
    state = state.copyWith(tool: tool, dirty: true);
    // Hive openBox is bound to a *local* variable here on purpose: the
    // P2 sidecar work tracks the box name through the variable so the
    // persists_to edge still resolves to the correct storage bucket.
    final box = await Hive.openBox('editor_state');
    box.put('lastTool', tool.toString());
  }

  Future<void> selectTool(EditorTool tool) async {
    await applyTool(tool);
  }

  void undo() {
    if (_undoStack.isEmpty) return;
    state = _undoStack.removeLast();
  }

  void openEditor(BuildContext context) {
    context.go('/editor');
  }
}

final editorProvider = StateNotifierProvider<EditorController, EditorState>(
  (ref) => EditorController(),
);
