//! Per-call cost of the settings accessors (`cargo bench --bench defaults_num`), the hot path of every `defaults::*` read
//! (#195). `daemon.queue` has an `env` override (the variable is unset here), `devswarm_wire.reconcile_min_gap_ms` has none.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report, and E2 exempts tests
use ah_engine::defaults;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn accessors(c: &mut Criterion) {
    defaults::init().unwrap();
    let mut g = c.benchmark_group("defaults");
    g.bench_function("num_env_key", |b| b.iter(|| defaults::num(black_box("daemon.queue"))));
    g.bench_function("num_plain_key", |b| b.iter(|| defaults::num(black_box("devswarm_wire.reconcile_min_gap_ms"))));
    g.bench_function("text", |b| b.iter(|| defaults::text(black_box("msg.cfg_err_io"))));
    g.finish();
}

criterion_group!(benches, accessors);
criterion_main!(benches);
