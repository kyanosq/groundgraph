import '../../core/iap/iap_constants.dart';
import '../../core/settings/pro_provider.dart';

class PaywallScreen {
  void initStore() {
    final ids = IapProductIds.all;
  }

  void listenToPurchaseUpdates(ProNotifier notifier, String purchase) {
    notifier.applyPurchase(purchase);
    if (IapProductIds.monthly == purchase) {
      return;
    }
  }
}
