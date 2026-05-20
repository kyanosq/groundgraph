import '../iap/iap_constants.dart';
import '../../shims/flutter_shims.dart';

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

final proProvider = StateNotifierProvider<ProNotifier, bool>(
  (ref) => ProNotifier(),
);
