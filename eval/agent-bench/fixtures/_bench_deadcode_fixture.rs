//! Benchmark fixture for the agent code-lookup benchmark (`eval/agent-bench`).
//!
//! Injected ONLY into the disposable bench worktree (never the real crate / CI)
//! by `setup_fixture.sh`. It defines an unreachable private cluster:
//! `bench_dead_beta` calls `bench_dead_alpha`, but nothing calls
//! `bench_dead_beta`, so BOTH are dead by whole-program reachability.
//!
//! The Rust compiler's `never used` lint is the INDEPENDENT oracle — it flags
//! both functions. A grep that only checks textual references is fooled into
//! thinking `bench_dead_alpha` is "used" (because `bench_dead_beta` references
//! it), which is exactly the reachability blind spot the benchmark probes.

fn bench_dead_alpha() -> u32 {
    41
}

fn bench_dead_beta() -> u32 {
    bench_dead_alpha().wrapping_add(1)
}
