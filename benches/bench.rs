//! Curated divan benchmarks for diff-pretty's hot paths, adapted from `~/d/e`'s
//! `benches/bench.rs` (divan + `AllocProfiler`, per-op median + alloc report).
//!
//! What is curated here:
//!
//! - **End-to-end `render()`** of every input class the product serves (git show
//!   corpus, multi-commit log, colorized log, plain unified diff) plus
//!   synthetic diffs scaled to 100KB / 1MB / 10MB. These are the LTO-relevant
//!   numbers: the whole pipeline fuses only under `lto = "fat"`.
//! - **Synthetic variants that isolate hot sub-paths** inside `render`: a
//!   colorized diff (SGR stripping) and a tab-heavy diff (`expand_tabs`).
//! - **Word-diff inference** (`edits::infer_edits` / Needleman-Wunsch), the
//!   quadratic hot spot and allocation pressure point: balanced, byte-identical
//!   (floor), the imbalanced greedy-pairing case from `TODO.md`, and a single
//!   long line (wide alignment table).
//! - **The per-line number primitive** (`config::pad_number`): every hunk line
//!   allocates through this, so it is a direct "alloc minimally" target.
//!
//! Setup (fixture loads, synthetic generation) happens once outside the
//! measured closure, so the medians and alloc counts are the steady-state cost
//! of one operation.

use std::path::PathBuf;
use std::sync::OnceLock;

use divan::{AllocProfiler, Bencher, black_box};

use diff_pretty::{config, edits, render};

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
            .filter(|e| e.path().extension().map_or(false, |x| x == "patch"))
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

// ---------------------------------------------------------------------------
// Synthetic diff generation (deterministic, `git show`-shaped, so the renderer
// walks the same code paths at scale)
// ---------------------------------------------------------------------------

fn push_line(buf: &mut String, line: &str, color: bool, code: &str) {
    if color {
        buf.push_str("\x1b[");
        buf.push_str(code);
        buf.push('m');
        buf.push_str(line);
        buf.push_str("\x1b[m");
    } else {
        buf.push_str(line);
    }
    buf.push('\n');
}

fn context_line(hunk: usize, i: usize, tabs: bool) -> String {
    let t = if tabs { "\t" } else { "" };
    match i % 3 {
        0 => format!("{t}    use std::io::{{self, BufRead, ErrorKind}};"),
        1 => format!("{t}    let input_{hunk} = std::env::args().collect::<Vec<_>>();"),
        _ => format!("{t}    let processed = pipeline_{hunk}(&input_{hunk})?;"),
    }
}

fn minus_line(hunk: usize, i: usize, tabs: bool) -> String {
    let t = if tabs { "\t" } else { "" };
    match i % 5 {
        0 => format!("{t}    let line_{hunk}_{i} = read_stdin_line()?;"),
        1 => format!("{t}    let tokens_{hunk}_{i} = tokenize(&line_{hunk}_{i});"),
        2 => format!("{t}    let expanded = expand_tabs(&tokens_{hunk}_{i}, {}) ;", hunk % 8),
        3 => format!("{t}    paint_line(&mut output, &tokens_{hunk}_{i});"),
        _ => format!("{t}    flush_sink(&mut sink, &tokens_{hunk}_{i});"),
    }
}

fn plus_line(hunk: usize, i: usize, tabs: bool) -> String {
    let t = if tabs { "\t" } else { "" };
    match i % 5 {
        0 => format!("{t}    let line_{hunk}_{i} = read_stdin_bytes()?;"),
        1 => format!("{t}    let tokens_{hunk}_{i} = tokenize_all(&line_{hunk}_{i});"),
        2 => format!("{t}    let expanded = expand_tabs(&tokens_{hunk}_{i}, {}) + 1;", hunk % 8),
        3 => format!("{t}    paint_line(&mut output, &tokens_{hunk}_{i});"),
        _ => format!("{t}    flush_sink(&mut sink, &tokens_{hunk}_{i});"),
    }
}

fn append_hunk(buf: &mut String, hunk: usize, color: bool, tabs: bool) {
    let base = hunk * 40 + 1;
    // Even hunks: balanced 5/5 (full word-diff pairing). Odd hunks: imbalanced
    // 7/2 (the greedy-pairing path from `TODO.md`).
    let (minus_count, plus_count) = if hunk % 2 == 0 { (5, 5) } else { (7, 2) };
    // Every 4th hunk carries a code fragment (drawn box); the rest have none.
    let frag = if hunk % 4 == 0 {
        format!(" fn process_{hunk}() -> Result<(), Error>")
    } else {
        String::new()
    };
    push_line(
        buf,
        &format!("@@ -{base},{minus_count} +{base},{plus_count} @@{frag}"),
        color,
        "36",
    );
    for i in 0..3 {
        let l = context_line(hunk, i, tabs);
        push_line(buf, &format!(" {l}"), color, "0");
    }
    for i in 0..minus_count {
        let m = minus_line(hunk, i, tabs);
        push_line(buf, &format!("-{m}"), color, "31");
    }
    for i in 0..plus_count {
        let p = plus_line(hunk, i, tabs);
        push_line(buf, &format!("+{p}"), color, "32");
    }
}

fn append_commit(buf: &mut String, file: usize, color: bool, tabs: bool, hunk: &mut usize) {
    let hash = format!("{file:040x}");
    push_line(buf, &format!("commit {hash}"), color, "33");
    push_line(buf, "Author:     Bench Builder <bench@example.com>", false, "");
    push_line(buf, "AuthorDate: Mon Aug 3 12:00:00 2026 -0400", false, "");
    push_line(buf, "Commit:     Bench Builder <bench@example.com>", false, "");
    push_line(buf, "CommitDate: Mon Aug 3 12:00:00 2026 -0400", false, "");
    buf.push('\n');
    push_line(buf, &format!("    synthesize module_{file:02}"), false, "");
    buf.push('\n');
    let path = format!("src/module_{file:02}/module_{file:02}.rs");
    push_line(buf, &format!("diff --git a/{path} b/{path}"), color, "1");
    push_line(buf, &format!("index 0000000..{file:07x} 100644"), color, "1");
    push_line(buf, &format!("--- a/{path}"), color, "1");
    push_line(buf, &format!("+++ b/{path}"), color, "1");
    for _ in 0..2 {
        append_hunk(buf, *hunk, color, tabs);
        *hunk += 1;
    }
}

/// Build a deterministic `git show`-shaped diff of at least `target` bytes.
fn make_diff(target: usize, color: bool, tabs: bool) -> String {
    let mut buf = String::with_capacity(target);
    let mut file = 0usize;
    let mut hunk = 0usize;
    while buf.len() < target {
        append_commit(&mut buf, file, color, tabs, &mut hunk);
        file += 1;
    }
    buf
}

fn once_leaked(cell: &OnceLock<&'static str>, make: impl FnOnce() -> String) -> &'static str {
    cell.get_or_init(|| Box::leak(make().into_boxed_str()))
}

fn synthetic_100kb() -> &'static str {
    static CACHE: OnceLock<&'static str> = OnceLock::new();
    once_leaked(&CACHE, || make_diff(100 * 1024, false, false))
}

fn synthetic_1mb() -> &'static str {
    static CACHE: OnceLock<&'static str> = OnceLock::new();
    once_leaked(&CACHE, || make_diff(1024 * 1024, false, false))
}

fn synthetic_10mb() -> &'static str {
    static CACHE: OnceLock<&'static str> = OnceLock::new();
    once_leaked(&CACHE, || make_diff(10 * 1024 * 1024, false, false))
}

fn synthetic_colorized_1mb() -> &'static str {
    static CACHE: OnceLock<&'static str> = OnceLock::new();
    once_leaked(&CACHE, || make_diff(1024 * 1024, true, false))
}

fn synthetic_tabs_1mb() -> &'static str {
    static CACHE: OnceLock<&'static str> = OnceLock::new();
    once_leaked(&CACHE, || make_diff(1024 * 1024, false, true))
}

// ---------------------------------------------------------------------------
// Word-diff / line-number fixtures
// ---------------------------------------------------------------------------

/// A line with many `\w+` tokens, so Needleman-Wunsch has a real table to fill.
fn code_line(i: usize) -> String {
    format!(
        "    let computed_value_{i} = scale_and_offset(input_{i}, {i}, {}) + adjust_{};",
        i % 7,
        i % 3
    )
}

/// One-token edit of `code_line`: everything else shared, so the distance stays
/// under the pairing threshold and the full matrix is filled.
fn mutate_line(line: &str) -> String {
    line.replacen("scale_and_offset", "scale_offset", 1)
}

fn edit_lines(minus_count: usize, plus_count: usize, similar: bool) -> (Vec<String>, Vec<String>) {
    let mut minus = Vec::with_capacity(minus_count);
    let mut plus = Vec::with_capacity(plus_count);
    for i in 0..minus_count {
        minus.push(code_line(i));
    }
    for i in 0..plus_count {
        let base = &minus[i % minus.len()];
        plus.push(if similar {
            base.clone()
        } else {
            mutate_line(base)
        });
    }
    (minus, plus)
}

// ---------------------------------------------------------------------------
// End-to-end render: real fixtures
// ---------------------------------------------------------------------------

#[divan::bench]
fn render_show_corpus(b: Bencher) {
    let patches = corpus();
    b.bench_local(|| {
        let mut bytes = 0usize;
        for (_, input) in patches {
            bytes = bytes.wrapping_add(render(input).len());
        }
        black_box(bytes)
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

// ---------------------------------------------------------------------------
// End-to-end render: synthetic scale + hot sub-paths
// ---------------------------------------------------------------------------

#[divan::bench]
fn render_synthetic_100kb(b: Bencher) {
    let input = synthetic_100kb();
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn render_synthetic_1mb(b: Bencher) {
    let input = synthetic_1mb();
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn render_synthetic_10mb(b: Bencher) {
    let input = synthetic_10mb();
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn render_synthetic_colorized_1mb(b: Bencher) {
    let input = synthetic_colorized_1mb();
    b.bench_local(|| black_box(render(input).len()));
}

#[divan::bench]
fn render_synthetic_tabs_1mb(b: Bencher) {
    let input = synthetic_tabs_1mb();
    b.bench_local(|| black_box(render(input).len()));
}

// ---------------------------------------------------------------------------
// Word-diff inference (the quadratic hot spot)
// ---------------------------------------------------------------------------

#[divan::bench]
fn infer_edits_balanced_200(b: Bencher) {
    let (minus, plus) = edit_lines(200, 200, false);
    let minus: Vec<&str> = minus.iter().map(String::as_str).collect();
    let plus: Vec<&str> = plus.iter().map(String::as_str).collect();
    b.bench_local(|| {
        let res = edits::infer_edits(&minus, &plus);
        black_box((res.minus_sections.len(), res.plus_sections.len()))
    });
}

#[divan::bench]
fn infer_edits_identical_200(b: Bencher) {
    let (minus, plus) = edit_lines(200, 200, true);
    let minus: Vec<&str> = minus.iter().map(String::as_str).collect();
    let plus: Vec<&str> = plus.iter().map(String::as_str).collect();
    b.bench_local(|| {
        let res = edits::infer_edits(&minus, &plus);
        black_box((res.minus_sections.len(), res.plus_sections.len()))
    });
}

#[divan::bench]
fn infer_edits_imbalanced_76_4(b: Bencher) {
    let (minus, plus) = edit_lines(76, 4, false);
    let minus: Vec<&str> = minus.iter().map(String::as_str).collect();
    let plus: Vec<&str> = plus.iter().map(String::as_str).collect();
    b.bench_local(|| {
        let res = edits::infer_edits(&minus, &plus);
        black_box((res.minus_sections.len(), res.plus_sections.len()))
    });
}

#[divan::bench]
fn infer_edits_long_line(b: Bencher) {
    let mut minus = String::new();
    let mut plus = String::new();
    for i in 0..25 {
        minus.push_str(&format!(" shared_token_{i}"));
        plus.push_str(&format!(" shared_token_{i}"));
    }
    plus.push_str(" extra_token");
    let minus = vec![minus.as_str()];
    let plus = vec![plus.as_str()];
    b.bench_local(|| {
        let res = edits::infer_edits(&minus, &plus);
        black_box((res.minus_sections.len(), res.plus_sections.len()))
    });
}

// ---------------------------------------------------------------------------
// Per-line number primitive (allocation pressure)
// ---------------------------------------------------------------------------

#[divan::bench]
fn pad_number_centered_10k(b: Bencher) {
    b.bench_local(|| {
        let mut bytes = 0usize;
        for n in 0..10_000 {
            bytes += config::pad_number(Some(n), 4).len();
        }
        black_box(bytes)
    });
}

fn main() {
    divan::main();
}
