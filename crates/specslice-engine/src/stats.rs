//! Per-command usage statistics.
//!
//! Every CLI invocation records one [`CommandStat`] (command name, wall-clock
//! duration, success flag, and any command-specific metrics) appended as a JSON
//! line to `<repo_root>/.specslice/stats.jsonl`. `specslice stats` then
//! aggregates the ledger into per-command call counts / total+avg+max duration /
//! error counts / summed metrics.
//!
//! Command runners push their own counters (e.g. search hits, indexed symbols,
//! port coverage) into a process-global collector via [`set_metric`] /
//! [`add_metric`]; the CLI drains them with [`take_metrics`] when the command
//! finishes, so no runner signature has to change to report numbers.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// File name (under `.specslice/`) of the append-only stats ledger.
pub const STATS_REL_PATH: &str = "stats.jsonl";

/// One recorded command invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandStat {
    /// Stable command name, e.g. `search`, `graph-equiv`, `candidate-review`.
    pub command: String,
    /// Unix epoch milliseconds when the record was made.
    #[serde(default)]
    pub ts_unix_ms: i64,
    /// Wall-clock duration of the command in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,
    /// Whether the command finished successfully (exit code 0, no error).
    #[serde(default)]
    pub ok: bool,
    /// Command-specific counters (nodes queried, results returned, coverage …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, i64>,
}

fn collector() -> &'static Mutex<BTreeMap<String, i64>> {
    static C: OnceLock<Mutex<BTreeMap<String, i64>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Add `value` to metric `key` for the current invocation (creates if absent).
pub fn add_metric(key: &str, value: i64) {
    if let Ok(mut m) = collector().lock() {
        *m.entry(key.to_string()).or_insert(0) += value;
    }
}

/// Set metric `key` to `value` for the current invocation (overwrites).
pub fn set_metric(key: &str, value: i64) {
    if let Ok(mut m) = collector().lock() {
        m.insert(key.to_string(), value);
    }
}

/// Drain and return all metrics recorded so far, resetting the collector.
pub fn take_metrics() -> BTreeMap<String, i64> {
    match collector().lock() {
        Ok(mut m) => std::mem::take(&mut *m),
        Err(_) => BTreeMap::new(),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Build a stat record stamped with the current time.
pub fn make_stat(
    command: &str,
    duration_ms: u64,
    ok: bool,
    metrics: BTreeMap<String, i64>,
) -> CommandStat {
    CommandStat {
        command: command.to_string(),
        ts_unix_ms: now_unix_ms(),
        duration_ms,
        ok,
        metrics,
    }
}

/// Append one stat as a JSON line to `<specslice_dir>/stats.jsonl`.
///
/// Best-effort and non-fatal: if `specslice_dir` does not exist (command run
/// outside a workspace, e.g. `graph-equiv` over bare db paths) it does nothing,
/// so statistics never break a command.
pub fn append_stat(specslice_dir: &Path, stat: &CommandStat) -> std::io::Result<()> {
    if !specslice_dir.is_dir() {
        return Ok(());
    }
    let path = specslice_dir.join(STATS_REL_PATH);
    let mut line = serde_json::to_string(stat)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())
}

/// Load all stats from a JSONL ledger. Missing file → empty; malformed lines are
/// skipped so a partially-written ledger is still summarisable.
pub fn load_stats(path: &Path) -> std::io::Result<Vec<CommandStat>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(stat) = serde_json::from_str::<CommandStat>(trimmed) {
            out.push(stat);
        }
    }
    Ok(out)
}

/// Per-command aggregate produced by [`summarize`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandSummary {
    pub command: String,
    pub calls: u64,
    pub errors: u64,
    pub total_ms: u64,
    pub avg_ms: f64,
    pub max_ms: u64,
    /// Metrics summed across all calls of this command.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, i64>,
}

/// Whole-ledger summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatsSummary {
    pub total_calls: u64,
    pub total_errors: u64,
    pub commands: Vec<CommandSummary>,
}

/// Aggregate raw stats into per-command summaries, sorted by call count
/// (descending), then command name (ascending) for stable output.
pub fn summarize(stats: &[CommandStat]) -> StatsSummary {
    let mut by: BTreeMap<String, CommandSummary> = BTreeMap::new();
    let mut total_calls = 0u64;
    let mut total_errors = 0u64;
    for s in stats {
        total_calls += 1;
        if !s.ok {
            total_errors += 1;
        }
        let entry = by
            .entry(s.command.clone())
            .or_insert_with(|| CommandSummary {
                command: s.command.clone(),
                calls: 0,
                errors: 0,
                total_ms: 0,
                avg_ms: 0.0,
                max_ms: 0,
                metrics: BTreeMap::new(),
            });
        entry.calls += 1;
        if !s.ok {
            entry.errors += 1;
        }
        entry.total_ms += s.duration_ms;
        entry.max_ms = entry.max_ms.max(s.duration_ms);
        for (k, v) in &s.metrics {
            *entry.metrics.entry(k.clone()).or_insert(0) += *v;
        }
    }
    let mut commands: Vec<CommandSummary> = by
        .into_values()
        .map(|mut c| {
            c.avg_ms = if c.calls > 0 {
                c.total_ms as f64 / c.calls as f64
            } else {
                0.0
            };
            c
        })
        .collect();
    commands.sort_by(|a, b| {
        b.calls
            .cmp(&a.calls)
            .then_with(|| a.command.cmp(&b.command))
    });
    StatsSummary {
        total_calls,
        total_errors,
        commands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // Single test for the process-global collector to avoid cross-test races.
    #[test]
    fn collector_add_set_take_resets() {
        let _ = take_metrics(); // clear any residue
        add_metric("hits", 3);
        add_metric("hits", 2);
        set_metric("nodes", 10);
        let m = take_metrics();
        assert_eq!(m.get("hits"), Some(&5));
        assert_eq!(m.get("nodes"), Some(&10));
        // draining resets
        assert!(take_metrics().is_empty());
    }

    #[test]
    fn append_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let ss = dir.path();
        let mut metrics = BTreeMap::new();
        metrics.insert("hits".to_string(), 7);
        let stat = make_stat("search", 12, true, metrics);
        append_stat(ss, &stat).unwrap();
        append_stat(ss, &make_stat("index", 100, true, BTreeMap::new())).unwrap();

        let loaded = load_stats(&ss.join(STATS_REL_PATH)).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].command, "search");
        assert_eq!(loaded[0].metrics.get("hits"), Some(&7));
        assert_eq!(loaded[1].command, "index");
    }

    #[test]
    fn append_is_noop_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        // Must not error and must not create the file.
        append_stat(&missing, &make_stat("search", 1, true, BTreeMap::new())).unwrap();
        assert!(!missing.join(STATS_REL_PATH).exists());
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let got = load_stats(&dir.path().join("absent.jsonl")).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn summarize_aggregates_calls_durations_and_metrics() {
        let stats = vec![
            make_stat("search", 10, true, btree(&[("hits", 3)])),
            make_stat("search", 30, false, btree(&[("hits", 5)])),
            make_stat("index", 100, true, btree(&[("symbols", 82)])),
        ];
        let sum = summarize(&stats);
        assert_eq!(sum.total_calls, 3);
        assert_eq!(sum.total_errors, 1);
        // search has more calls -> first.
        assert_eq!(sum.commands[0].command, "search");
        let search = &sum.commands[0];
        assert_eq!(search.calls, 2);
        assert_eq!(search.errors, 1);
        assert_eq!(search.total_ms, 40);
        assert_eq!(search.max_ms, 30);
        assert!((search.avg_ms - 20.0).abs() < 1e-9);
        assert_eq!(search.metrics.get("hits"), Some(&8));
        let index = &sum.commands[1];
        assert_eq!(index.calls, 1);
        assert_eq!(index.metrics.get("symbols"), Some(&82));
    }

    #[test]
    fn load_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.jsonl");
        std::fs::write(
            &path,
            "{\"command\":\"search\",\"duration_ms\":5,\"ok\":true}\nNOT JSON\n\n",
        )
        .unwrap();
        let loaded = load_stats(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].command, "search");
    }

    fn btree(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }
}
