import '../../lib/features/editor/editor_controller.dart';

void main() {
  group('EditorController', () {
    test('applyTool flips dirty and remembers undo state', () async {
      final controller = EditorController();
      await controller.applyTool(EditorTool.eraser);
      assert(controller.state.tool == EditorTool.eraser);
      assert(controller.state.dirty == true);
    });

    test('undo restores the previous state', () async {
      final controller = EditorController();
      await controller.applyTool(EditorTool.eraser);
      controller.undo();
      assert(controller.state.tool == EditorTool.brush);
    });
  });
}

void group(String name, void Function() body) => body();
void test(String name, void Function() body) => body();
