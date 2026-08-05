//! Curated divan benchmarks for diff-pretty's hot paths.
//!
//! What is curated here:
//!
//! - **End-to-end rendering** of representative Git-show, metadata-heavy,
//!   multi-commit, colorized, and plain unified-diff fixtures.
//! - **Real streaming and retained-document paths** over those same fixtures.
//! - **Real pager paths** using a checked-in document for viewport rendering and
//!   search setup/redraw.
//!
//! The real corpus is split into typical, metadata-heavy, and large bundles so
//! each practical shape is visible instead of being hidden by one aggregate.
//! Setup (fixture loads) happens once outside the measured closure, so the
//! medians and alloc counts are the steady-state cost
//! of one operation.

use std::path::PathBuf;
use std::sync::OnceLock;

use divan::{AllocProfiler, Bencher, black_box};

use diff_pretty::{render, render_document, render_reader_to};

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

// ---------------------------------------------------------------------------
// Real vendored fixtures (loaded once, leaked for the process lifetime)
// ---------------------------------------------------------------------------

/// Every `fixtures/*.patch`, sorted by name. Used directly by the corpus bench
/// and as the source for individual named fixtures.
fn corpus() -> &'static [(String, String)] {
    static CORPUS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut patches: Vec<(String, String)> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "patch"))
            .map(|e| {
                let stem = e.path().file_stem().unwrap().to_str().unwrap().to_string();
                let input = std::fs::read_to_string(e.path()).unwrap();
                (stem, input)
            })
            .collect();
        patches.sort_by(|a, b| a.0.cmp(&b.0));
        patches
    })
}

fn fixture(name: &str) -> &'static str {
    corpus()
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, s)| s.as_str())
        .unwrap_or_else(|| panic!("fixture {name} not in corpus"))
}

// These are deliberately named rather than selected by a size threshold: the
// benchmark corpus is a checked-in presentation contract, and a fixture moving
// between buckets should be an intentional reviewable change.
const SHOW_METADATA_FIXTURES: &[&str] = &[
    "show_001", "show_003", "show_006", "show_032", "show_038", "show_039", "show_040", "show_063",
    "show_078", "show_082", "show_095", "show_098",
];

const SHOW_LARGE_FIXTURES: &[&str] = &[
    "show_015", "show_041", "show_048", "show_059", "show_079", "show_084", "show_088", "show_096",
    "show_099",
];

fn render_named_fixtures(names: &[&str]) -> usize {
    names.iter().map(|name| render(fixture(name)).len()).sum()
}

fn render_typical_show_fixtures() -> usize {
    corpus()
        .iter()
        .filter(|(name, _)| {
            name.starts_with("show_")
                && !SHOW_METADATA_FIXTURES.contains(&name.as_str())
                && !SHOW_LARGE_FIXTURES.contains(&name.as_str())
        })
        .map(|(_, input)| render(input).len())
        .sum()
}

// ---------------------------------------------------------------------------
// End-to-end render: real fixtures
// ---------------------------------------------------------------------------

#[divan::bench]
fn render_show_typical(b: Bencher) {
    b.bench_local(|| black_box(render_typical_show_fixtures()));
}

#[divan::bench]
fn render_show_metadata(b: Bencher) {
    b.bench_local(|| black_box(render_named_fixtures(SHOW_METADATA_FIXTURES)));
}

#[divan::bench]
fn render_show_large(b: Bencher) {
    b.bench_local(|| black_box(render_named_fixtures(SHOW_LARGE_FIXTURES)));
}

#[divan::bench]
fn render_show_corpus(b: Bencher) {
    b.bench_local(|| {
        black_box(
            render_typical_show_fixtures()
                .wrapping_add(render_named_fixtures(SHOW_METADATA_FIXTURES))
                .wrapping_add(render_named_fixtures(SHOW_LARGE_FIXTURES)),
        )
    });
}

#[divan::bench]
fn render_document_show_010(b: Bencher) {
    let input = fixture("show_010");
    b.bench_local(|| black_box(render_document(input)));
}

#[divan::bench]
fn render_reader_to_log_000(b: Bencher) {
    let input = fixture("log_000");
    b.bench_local(|| {
        let mut output = std::io::sink();
        render_reader_to(input.as_bytes(), &mut output).expect("sink writes cannot fail");
        black_box(output)
    });
}

#[divan::bench]
fn render_log_000(b: Bencher) {
    let input = fixture("log_000");
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn render_log_000_color(b: Bencher) {
    let input = fixture("log_000_color");
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn render_plain_unified(b: Bencher) {
    let input = fixture("plain_unified");
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn pager_viewport_show_010(b: Bencher) {
    let document = render_document(fixture("show_010"));
    let mut terminal = Vec::with_capacity(16 * 1024);
    b.bench_local(|| {
        terminal.clear();
        document
            .write_viewport(&mut terminal, 0, 24)
            .expect("writing to a Vec cannot fail");
        black_box(terminal.len())
    });
}

fn main() {
    divan::main();
}
