import '../iap/iap_constants.dart';

class ProNotifier {
  bool state = false;

  Future<void> applyPurchase(String productId) async {
    if (IapProductIds.all.contains(productId)) {
      state = true;
    }
  }
}
