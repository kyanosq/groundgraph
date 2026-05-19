import '../iap/iap_constants.dart';

// Riverpod-like surface — the real Pixcraft uses package:flutter_riverpod, but
// for this fixture we expose just enough vocabulary that `package:analyzer`
// resolves the names and our P8 walker can pattern-match Provider construction
// and ref.read/watch invocations.
class StateNotifierProvider<N, T> {
  final N Function(dynamic ref) build;
  StateNotifierProvider(this.build);
}

class Ref {
  T read<T>(Object provider) => null as T;
  T watch<T>(Object provider) => null as T;
  void listen<T>(Object provider, void Function(T?, T) listener) {}
}

class ProNotifier {
  bool state = false;

  Future<void> applyPurchase(String productId) async {
    if (IapProductIds.all.contains(productId)) {
      state = true;
    }
  }

  Future<void> restorePurchases() async {
    // Intentionally incomplete — the audit step should notice receipt
    // verification + expiry checking are missing.
  }
}

// Top-level Riverpod provider — P8 must pick this up as `dart_provider`.
final proProvider = StateNotifierProvider<ProNotifier, bool>(
  (ref) => ProNotifier(),
);
