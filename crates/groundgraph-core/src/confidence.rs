//! `Confidence` newtype: a `f32` guaranteed finite and within `[0.0, 1.0]`.
//!
//! Issues.md #168 / #63 (construction side): a bare `pub confidence: f32`
//! cannot express the `[0, 1]` invariant, so callers could build edges with
//! `NaN` (breaks total ordering / panics `partial_cmp`-based sorts) or
//! out-of-range scores. `Confidence` makes the invariant a type property:
//! every value that exists is already valid, so downstream sorting and
//! comparison needs no defensive clamps.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::edge::sanitize_confidence;

/// A confidence score guaranteed to be a finite `f32` in `[0.0, 1.0]`.
///
/// Wire format is unchanged from the previous bare `f32`: it serialises as a
/// plain JSON/YAML number, and deserialisation sanitises (same semantics as
/// [`sanitize_confidence`]) so hand-edited `candidates.yaml` values like
/// `confidence: 2.0` or `.nan` are folded into range instead of poisoning the
/// graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confidence(f32);

impl Confidence {
    /// Full confidence — the value the `declared` / `fact` edge factories use.
    pub const FULL: Confidence = Confidence(1.0);
    /// No confidence.
    pub const ZERO: Confidence = Confidence(0.0);
    /// Even odds — the documented default for a missing candidate confidence.
    pub const HALF: Confidence = Confidence(0.5);

    /// Sanitising constructor: `NaN → 1.0`, `±∞` and out-of-range finites
    /// clamp to the bounds, `-0.0` normalises to `0.0`. Mirrors
    /// [`sanitize_confidence`] — an edge exists because some indexer asserted
    /// it, so an unrepresentable value folds to full confidence rather than
    /// failing construction.
    #[inline]
    pub fn new(value: f32) -> Self {
        let sanitised = sanitize_confidence(value);
        // Normalise -0.0 so `Eq`-style comparisons and any future `Hash` impl
        // see a single zero representation.
        let sanitised = if sanitised == 0.0 { 0.0 } else { sanitised };
        Confidence(sanitised)
    }

    /// Strict constructor: rejects `NaN`, infinities, and out-of-range values.
    /// Use at trust boundaries where silently folding a bad value would hide
    /// a producer bug (e.g. decoding a store row written by the current
    /// build).
    #[inline]
    pub fn try_new(value: f32) -> Result<Self, InvalidConfidence> {
        if value.is_nan() {
            return Err(InvalidConfidence::Nan);
        }
        if value.is_infinite() {
            return Err(InvalidConfidence::Infinite);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(InvalidConfidence::OutOfRange(value));
        }
        Ok(Confidence::new(value))
    }

    /// The guaranteed-finite value in `[0.0, 1.0]`.
    #[inline]
    pub fn get(self) -> f32 {
        self.0
    }
}

/// Why a [`Confidence::try_new`] call rejected its input.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum InvalidConfidence {
    #[error("confidence is NaN")]
    Nan,
    #[error("confidence is infinite")]
    Infinite,
    #[error("confidence {0} is outside [0.0, 1.0]")]
    OutOfRange(f32),
}

impl Default for Confidence {
    /// A missing confidence means "the edge exists because an indexer
    /// asserted it" — full confidence, matching the edge factories.
    #[inline]
    fn default() -> Self {
        Confidence::FULL
    }
}

impl From<Confidence> for f32 {
    #[inline]
    fn from(c: Confidence) -> f32 {
        c.0
    }
}

impl From<Confidence> for f64 {
    #[inline]
    fn from(c: Confidence) -> f64 {
        f64::from(c.0)
    }
}

/// Total ordering: the contained value is never `NaN`, so this cannot panic.
impl PartialOrd for Confidence {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.0.total_cmp(&other.0))
    }
}

// Convenience comparisons against bare float literals keep call sites and
// test assertions readable (`assert_eq!(edge.confidence, 1.0)`).
impl PartialEq<f32> for Confidence {
    #[inline]
    fn eq(&self, other: &f32) -> bool {
        self.0 == *other
    }
}

impl PartialOrd<f32> for Confidence {
    #[inline]
    fn partial_cmp(&self, other: &f32) -> Option<std::cmp::Ordering> {
        Some(self.0.total_cmp(other))
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for Confidence {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Wire contract: a plain number, identical to the old bare `f32`.
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Sanitise on the way in: hand-edited YAML may carry `.nan` / `.inf`
        // or out-of-range numbers (issues.md #63 trigger scenario).
        Ok(Confidence::new(f32::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sanitises_like_sanitize_confidence() {
        assert_eq!(Confidence::new(0.0).get(), 0.0);
        assert_eq!(Confidence::new(0.78).get(), 0.78);
        assert_eq!(Confidence::new(1.0).get(), 1.0);
        assert_eq!(Confidence::new(-0.5).get(), 0.0);
        assert_eq!(Confidence::new(1.5).get(), 1.0);
        assert_eq!(Confidence::new(1e30).get(), 1.0);
        assert_eq!(Confidence::new(f32::INFINITY).get(), 1.0);
        assert_eq!(Confidence::new(f32::NEG_INFINITY).get(), 0.0);
        assert_eq!(Confidence::new(f32::NAN).get(), 1.0);
        for raw in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 9.0, 0.4] {
            let v = Confidence::new(raw).get();
            assert!(v.is_finite() && (0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn new_normalises_negative_zero() {
        let c = Confidence::new(-0.0_f32);
        assert_eq!(c.get().to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn try_new_rejects_unrepresentable_values() {
        assert_eq!(Confidence::try_new(f32::NAN), Err(InvalidConfidence::Nan));
        assert_eq!(
            Confidence::try_new(f32::INFINITY),
            Err(InvalidConfidence::Infinite)
        );
        assert_eq!(
            Confidence::try_new(f32::NEG_INFINITY),
            Err(InvalidConfidence::Infinite)
        );
        assert_eq!(
            Confidence::try_new(1.5),
            Err(InvalidConfidence::OutOfRange(1.5))
        );
        assert!(matches!(
            Confidence::try_new(-0.25),
            Err(InvalidConfidence::OutOfRange(_))
        ));
        assert_eq!(Confidence::try_new(0.42).unwrap().get(), 0.42);
    }

    #[test]
    fn ordering_is_total_and_sortable() {
        let mut values = [
            Confidence::new(0.9),
            Confidence::ZERO,
            Confidence::new(f32::NAN), // sanitised to 1.0
            Confidence::HALF,
        ];
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let got: Vec<f32> = values.iter().map(|c| c.get()).collect();
        assert_eq!(got, vec![0.0, 0.5, 0.9, 1.0]);
    }

    #[test]
    fn compares_against_bare_f32_literals() {
        let c = Confidence::new(0.5);
        assert_eq!(c, 0.5);
        assert!(c > 0.4);
        assert!(c < 0.6);
    }

    #[test]
    fn default_is_full_confidence() {
        assert_eq!(Confidence::default(), Confidence::FULL);
    }

    #[test]
    fn serde_round_trip_keeps_plain_number_wire_format() {
        let c = Confidence::new(0.78);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "0.78");
        let back: Confidence = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn deserialisation_sanitises_out_of_range_yaml() {
        // Hand-edited candidates.yaml with an out-of-range value folds into
        // range at the type level instead of being clamped ad hoc downstream.
        let c: Confidence = serde_norway::from_str("2.0").unwrap();
        assert_eq!(c, 1.0);
        let c: Confidence = serde_norway::from_str("-0.5").unwrap();
        assert_eq!(c, 0.0);
    }

    #[test]
    fn converts_into_wider_floats() {
        let c = Confidence::new(0.25);
        assert_eq!(f32::from(c), 0.25);
        assert_eq!(f64::from(c), 0.25);
    }
}
