import '../../core/iap/iap_constants.dart';
import '../../core/settings/pro_provider.dart';

// Lightweight stand-ins for the Flutter / GoRouter / Hive surface that the
// real Pixcraft codebase pulls from external packages. The names are
// intentional: `Hive.box`, `context.push`, `Stream<T>.listen` — the P8
// walker recognises them by name + static type.
class Hive {
  static dynamic box(String name) => _Box(name);
}

class _Box {
  final String name;
  _Box(this.name);
  void put(String key, Object value) {}
  dynamic get(String key) => null;
  void delete(String key) {}
}

class BuildContext {
  void push(String route) {}
  void go(String route) {}
  void pushNamed(String route) {}
}

class PurchaseUpdate {
  final String productId;
  PurchaseUpdate(this.productId);
}

class PurchaseStream {
  Stream<PurchaseUpdate> get updates =>
      Stream<PurchaseUpdate>.fromIterable(const <PurchaseUpdate>[]);
}

class PaywallScreen {
  void initStore(Ref ref) {
    final ids = IapProductIds.all;
  }

  void listenToPurchaseUpdates(
    Ref ref,
    PurchaseStream service,
    BuildContext context,
  ) {
    // ref.read on a Riverpod provider — P8 reads_provider edge expected.
    final notifier = ref.read<ProNotifier>(proProvider);
    notifier.applyPurchase(IapProductIds.monthly);

    // Hive.box(...).put(...) — P8 persists_to edge expected.
    Hive.box('pro_entitlement').put('isPro', true);

    // context.push(...) — P8 navigates_to edge expected.
    context.push('/paywall_thanks');

    // stream.listen(...) — P8 subscribes_stream edge expected.
    service.updates.listen((PurchaseUpdate update) {
      notifier.applyPurchase(update.productId);
    });
  }
}
