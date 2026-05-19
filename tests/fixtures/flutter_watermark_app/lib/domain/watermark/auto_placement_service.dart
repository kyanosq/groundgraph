import 'placement_candidate.dart';

class AutoPlacementService {
  AutoPlacementService();

  PlacementCandidate placeWatermark(List<PlacementCandidate> candidates) {
    candidates.sort((a, b) => b.score.compareTo(a.score));
    return candidates.first;
  }

  double scoreCandidate(PlacementCandidate candidate) {
    return candidate.score;
  }
}
