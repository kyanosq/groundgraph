//! Coarse index-phase progress reporting (issues.md #231).
//!
//! `groundgraph index` on a large repo (spring-framework / django) is silent
//! for tens of seconds before its summary dump. To surface progress without
//! coupling the engine to a terminal library, the indexer calls into a
//! [`ProgressSink`] once per phase (docs → each language → scip → links →
//! fulltext → commit). The CLI installs an `indicatif` sink that draws a
//! spinner on a TTY and stays quiet under CI; the default [`NoopSink`] makes
//! library callers (tests, embedders) pay nothing and keeps the engine free
//! of any TTY/`indicatif` dependency.

/// Receiver of coarse index-phase progress events.
///
/// One `phase` call marks the boundary *between* phases — the sink learns
/// "the docs pass just started" / "the scip pass just started", which is all
/// a spinner needs. Per-file granularity is intentionally not modelled here
/// (it would couple the sink to each adapter's inner loop).
pub trait ProgressSink {
    /// The phase that is starting, e.g. `"docs"`, `"dart"`, `"scip"`,
    /// `"fulltext"`, `"commit"`.
    fn phase(&mut self, phase: &str);
}

/// Sink that discards every event — the default for library callers and for
/// `index_repository` (which delegates to
/// [`index_repository_with_progress`](crate::index_repository_with_progress)
/// with a `NoopSink`).
#[derive(Debug, Default)]
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn phase(&mut self, _phase: &str) {}
}

/// In-memory recorder used by tests to assert the phase sequence the indexer
/// emits. Not used by production code.
#[derive(Debug, Default)]
pub struct RecordingSink {
    /// Every phase name reported, in call order.
    pub phases: Vec<String>,
}

impl ProgressSink for RecordingSink {
    fn phase(&mut self, phase: &str) {
        self.phases.push(phase.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_sink_collects_phases_in_order() {
        let mut sink = RecordingSink::default();
        sink.phase("docs");
        sink.phase("commit");
        assert_eq!(sink.phases, vec!["docs".to_string(), "commit".into()]);
    }

    #[test]
    fn noop_sink_accepts_phases_without_panic() {
        let mut sink = NoopSink;
        // A no-op sink must still satisfy the trait contract.
        sink.phase("docs");
        sink.phase("scip");
        sink.phase("commit");
    }

    #[test]
    fn trait_object_dispatches_to_concrete_recording_sink() {
        // The indexer passes `&mut dyn ProgressSink`; exercise that path.
        let mut sink = RecordingSink::default();
        {
            let dyn_sink: &mut dyn ProgressSink = &mut sink;
            dyn_sink.phase("docs");
            dyn_sink.phase("fulltext");
        }
        assert_eq!(
            sink.phases,
            vec!["docs".to_string(), "fulltext".to_string()]
        );
    }
}
