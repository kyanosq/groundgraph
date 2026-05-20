import '../../core/iap/iap_constants.dart';
import '../../core/settings/pro_provider.dart';
import '../../shims/flutter_shims.dart';

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
    final notifier = ref.read<ProNotifier>(proProvider);
    notifier.applyPurchase(IapProductIds.monthly);

    Hive.box('pro_entitlement').put('isPro', true);

    context.push('/paywall_thanks');

    service.updates.listen((PurchaseUpdate update) {
      notifier.applyPurchase(update.productId);
    });
  }
}
