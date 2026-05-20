import '../../lib/core/iap/iap_constants.dart';

void main() {
  group('IapProductIds', () {
    test('exposes monthly/yearly/lifetime ids', () {
      assert(IapProductIds.monthly == 'pro_monthly');
      assert(IapProductIds.yearly == 'pro_yearly');
      assert(IapProductIds.lifetime == 'pro_lifetime');
      assert(IapProductIds.all.length == 3);
    });

    test('all is in monthly/yearly/lifetime order', () {
      final ids = IapProductIds.all;
      assert(ids.first == IapProductIds.monthly);
      assert(ids.last == IapProductIds.lifetime);
    });
  });
}

void group(String name, void Function() body) => body();
void test(String name, void Function() body) => body();
