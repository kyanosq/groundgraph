// Shared Flutter / Riverpod / Hive / GoRouter shims for the Pixcraft
// fixture. The real Pixcraft app pulls these from external packages;
// because this fixture has no pubspec.yaml we re-declare just enough of
// the surface for `package:analyzer` to resolve names and for the P8
// pattern matcher to recognise:
//
//   * `Hive.box('name').put / .get / .delete`
//   * `Hive.openBox('name')` (used in layer/project lifecycle code)
//   * `context.push / .go / .pushNamed / .goNamed`
//   * `StateNotifierProvider<N, T>` (Riverpod-shaped provider)
//   * `Ref.read / .watch / .listen`
//   * `Stream<T>.listen` subscriptions
//
// Keeping all shims in one place avoids each feature file redeclaring
// (and conflicting on) `class Hive` / `class BuildContext`.

class Hive {
  static dynamic box(String name) => _Box(name);
  static Future<_Box> openBox(String name) async => _Box(name);
}

class _Box {
  final String name;
  _Box(this.name);
  void put(String key, Object value) {}
  dynamic get(String key) => null;
  void delete(String key) {}
  void clear() {}
}

class BuildContext {
  void push(String route) {}
  void go(String route) {}
  void pushNamed(String route) {}
  void goNamed(String route) {}
  void pushReplacement(String route) {}
}

class StateNotifierProvider<N, T> {
  final N Function(dynamic ref) build;
  StateNotifierProvider(this.build);
}

class Provider<T> {
  final T Function(dynamic ref) build;
  Provider(this.build);
}

class Ref {
  T read<T>(Object provider) => null as T;
  T watch<T>(Object provider) => null as T;
  void listen<T>(Object provider, void Function(T?, T) listener) {}
}
