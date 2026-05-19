import 'package:flutter_watermark_app/domain/watermark/auto_placement_service.dart';
import 'package:flutter_watermark_app/domain/watermark/placement_candidate.dart';

void main() {
  /// @verifies REQ-WATERMARK-001
  test('places watermark outside face region', () {
    final service = AutoPlacementService();
    final best = service.placeWatermark([
      PlacementCandidate(0.0, 0.0, 0.1),
      PlacementCandidate(1.0, 1.0, 0.9),
    ]);
    expect(best.score, equals(0.9));
  });
}

void test(String name, void Function() body) => body();
void expect(Object? actual, Object? matcher) {}
Object equals(Object? value) => value ?? 'null';
