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

/// Rotate the ledger once it reaches this size so an append-only file on a
/// long-lived repo (years of CI) cannot grow without bound and force a huge
/// read at `specslice stats` time (#250). The previous ledger is kept as a
/// single `.1` sibling, so at most ~2× this is retained on disk.
pub const MAX_STATS_BYTES: u64 = 8 * 1024 * 1024;

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
    append_stat_inner(specslice_dir, stat, MAX_STATS_BYTES)
}

/// [`append_stat`] with an injectable rotation cap (so tests can exercise
/// rotation without writing megabytes).
fn append_stat_inner(
    specslice_dir: &Path,
    stat: &CommandStat,
    max_bytes: u64,
) -> std::io::Result<()> {
    if !specslice_dir.is_dir() {
        return Ok(());
    }
    let path = specslice_dir.join(STATS_REL_PATH);
    // Rotate before appending if the ledger has reached the cap. Best-effort:
    // a lost race between concurrent processes at worst drops a handful of
    // best-effort stat lines, never breaks a command.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() >= max_bytes {
            let rotated = specslice_dir.join(format!("{STATS_REL_PATH}.1"));
            let _ = std::fs::rename(&path, &rotated);
        }
    }
    let mut line = serde_json::to_string(stat)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    // Exclusive lock for the append: `write_all` may split into several
    // `write` syscalls (large metrics maps), and concurrent specslice
    // processes (CI: index + search + impact on one repo) would interleave
    // bytes mid-line (issues2.md #35). Released when `f` drops.
    f.lock()?;
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

/// Incremental ledger aggregator. Folding one [`CommandStat`] at a time keeps
/// memory at O(distinct commands) regardless of ledger size, so [`summarize_file`]
/// can stream a multi-MB `stats.jsonl` instead of buffering every record (#250).
#[derive(Default)]
struct Aggregator {
    by: BTreeMap<String, CommandSummary>,
    total_calls: u64,
    total_errors: u64,
}

impl Aggregator {
    fn add(&mut self, s: &CommandStat) {
        self.total_calls += 1;
        if !s.ok {
            self.total_errors += 1;
        }
        let entry = self
            .by
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

    fn finish(self) -> StatsSummary {
        let mut commands: Vec<CommandSummary> = self
            .by
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
            total_calls: self.total_calls,
            total_errors: self.total_errors,
            commands,
        }
    }
}

/// Aggregate raw stats into per-command summaries, sorted by call count
/// (descending), then command name (ascending) for stable output.
pub fn summarize(stats: &[CommandStat]) -> StatsSummary {
    let mut agg = Aggregator::default();
    for s in stats {
        agg.add(s);
    }
    agg.finish()
}

/// Stream a single JSONL ledger into a [`StatsSummary`] without materialising
/// every record — the memory-safe path for `specslice stats` (#250).
pub fn summarize_file(path: &Path) -> std::io::Result<StatsSummary> {
    summarize_files(&[path])
}

/// Stream several ledgers (e.g. rotated `stats.jsonl.1` + current `stats.jsonl`)
/// into one summary. Missing files are skipped; malformed lines are ignored so a
/// partially-written ledger is still summarisable.
pub fn summarize_files(paths: &[&Path]) -> std::io::Result<StatsSummary> {
    let mut agg = Aggregator::default();
    for path in paths {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for line in BufReader::new(file).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(stat) = serde_json::from_str::<CommandStat>(trimmed) {
                agg.add(&stat);
            }
        }
    }
    Ok(agg.finish())
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

    /// issues2.md #35: concurrent appends from several processes (CI
    /// running index + search + impact at once) must never interleave
    /// bytes mid-line. Large metrics maps force multi-syscall writes;
    /// the file lock keeps each line whole.
    #[test]
    fn concurrent_appends_never_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        let ss = dir.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let ss = ss.clone();
                std::thread::spawn(move || {
                    // ~64 KB of metrics per line to defeat single-write atomicity.
                    let mut metrics = BTreeMap::new();
                    for k in 0..1500 {
                        metrics.insert(format!("metric_thread{t}_key{k:05}"), k);
                    }
                    for i in 0..20u64 {
                        let stat = make_stat(&format!("cmd{t}_{i}"), i, true, metrics.clone());
                        append_stat(&ss, &stat).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let loaded = load_stats(&ss.join(STATS_REL_PATH)).unwrap();
        assert_eq!(
            loaded.len(),
            8 * 20,
            "every appended line must parse back — corruption loses lines"
        );
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

    /// #250: the summary must be computable by streaming the ledger so a
    /// multi-MB `stats.jsonl` never has to be materialised into a `Vec` first.
    #[test]
    fn summarize_file_streams_to_same_result_as_load_plus_summarize() {
        let dir = tempfile::tempdir().unwrap();
        let ss = dir.path();
        append_stat(ss, &make_stat("search", 10, true, btree(&[("hits", 3)]))).unwrap();
        append_stat(ss, &make_stat("search", 30, false, btree(&[("hits", 5)]))).unwrap();
        append_stat(
            ss,
            &make_stat("index", 100, true, btree(&[("symbols", 82)])),
        )
        .unwrap();

        let path = ss.join(STATS_REL_PATH);
        let streamed = summarize_file(&path).unwrap();
        let materialised = summarize(&load_stats(&path).unwrap());
        assert_eq!(streamed, materialised);
        assert_eq!(streamed.total_calls, 3);
        assert_eq!(streamed.commands[0].command, "search");
        assert_eq!(streamed.commands[0].metrics.get("hits"), Some(&8));
    }

    /// #250: after rotation the recent history lives across `stats.jsonl.1`
    /// (rotated) + `stats.jsonl` (current); the summary must fold both.
    #[test]
    fn summarize_files_merges_rotated_and_current() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("stats.jsonl.1");
        let p0 = dir.path().join("stats.jsonl");
        std::fs::write(
            &p1,
            serde_json::to_string(&make_stat("search", 5, true, BTreeMap::new())).unwrap() + "\n",
        )
        .unwrap();
        std::fs::write(
            &p0,
            serde_json::to_string(&make_stat("index", 9, true, BTreeMap::new())).unwrap() + "\n",
        )
        .unwrap();
        let sum = summarize_files(&[p1.as_path(), p0.as_path()]).unwrap();
        assert_eq!(sum.total_calls, 2);
        // A missing path is skipped, not an error.
        let only =
            summarize_files(&[dir.path().join("absent.jsonl").as_path(), p0.as_path()]).unwrap();
        assert_eq!(only.total_calls, 1);
    }

    /// #250: an unbounded append-only ledger must rotate so it cannot grow
    /// without limit. Past the byte cap, the old ledger moves to `.1` and the
    /// current file restarts with just the new record.
    #[test]
    fn append_rotates_when_ledger_exceeds_cap() {
        let dir = tempfile::tempdir().unwrap();
        let ss = dir.path();
        let path = ss.join(STATS_REL_PATH);
        // Seed an over-cap ledger (content is irrelevant; rotation is by size).
        std::fs::write(&path, b"{\"command\":\"old\",\"ok\":true}\n").unwrap();

        append_stat_inner(ss, &make_stat("search", 1, true, BTreeMap::new()), 8).unwrap();

        let rotated = ss.join(format!("{STATS_REL_PATH}.1"));
        assert!(rotated.exists(), "over-cap ledger must rotate to .1");
        let current = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            current.lines().count(),
            1,
            "current ledger restarts with only the new line after rotation"
        );
        assert!(current.contains("search"));
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
