//! Focused scrl pager benchmarks.
//!
//! These deliberately measure the work v1 is expected to change: complete
//! viewport redraws, horizontal clipping, cached search highlighting, and a
//! live document receiving a new chunk while search is active. Setup happens
//! outside the steady-state redraw benchmarks. Divan's allocator profiler
//! reports time, allocation count, and allocated bytes for each operation.

use divan::{AllocProfiler, Bencher, black_box};
use scrl::{Document, DocumentBuilder, Event, Session, SessionOptions, Size};

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn corpus(lines: usize) -> String {
    let mut input = String::with_capacity(lines * 96);
    for line in 0..lines {
        input.push_str("commit ");
        input.push_str(&format!("{line:08x}"));
        input.push_str(" function render_document changed_token_0001 changed_token_0002\n");
    }
    input
}

fn styled_corpus(lines: usize) -> String {
    let mut input = String::with_capacity(lines * 120);
    for line in 0..lines {
        input.push_str("\x1b[31mremoved ");
        input.push_str(&format!("{line:08x}"));
        input.push_str("\x1b[0m context ");
        input.push_str("\x1b[32madded changed_token_0001\x1b[0m\n");
    }
    input
}

fn document(input: &str) -> Document {
    let mut builder = DocumentBuilder::new();
    builder.push_str(input);
    builder.finish()
}

fn session(document_text: &str) -> Session {
    let mut pager = Session::new(
        Size {
            rows: 24,
            columns: 100,
        },
        SessionOptions {
            title: "bench".into(),
            search_history: Vec::new(),
            wrap: false,
            follow: false,
            filter: None,
        },
    );
    pager.push_chunk(document_text);
    pager.finish();
    pager
}

#[divan::bench]
fn viewport_redraw_no_search(bencher: Bencher) {
    let document = document(&corpus(20_000));
    let mut frame = Vec::with_capacity(16 * 1024);
    bencher.bench_local(|| {
        frame.clear();
        document.write_viewport(&mut frame, 2_000, 24).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn viewport_redraw_styled(bencher: Bencher) {
    let document = document(&styled_corpus(20_000));
    let mut frame = Vec::with_capacity(16 * 1024);
    bencher.bench_local(|| {
        frame.clear();
        document.write_viewport(&mut frame, 2_000, 24).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn viewport_redraw_horizontal_clip(bencher: Bencher) {
    let mut pager = session(&corpus(20_000));
    pager.handle(Event::Right);
    let mut frame = Vec::with_capacity(16 * 1024);
    bencher.bench_local(|| {
        frame.clear();
        pager.draw(&mut frame).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn session_redraw_without_movement(bencher: Bencher) {
    let text = corpus(20_000);
    let mut pager = session(&text);
    let mut frame = Vec::with_capacity(16 * 1024);
    bencher.bench_local(|| {
        frame.clear();
        pager.draw(&mut frame).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn initial_search_late_match(bencher: Bencher) {
    let text = corpus(20_000);
    bencher.bench_local(|| {
        let mut pager = session(&text);
        pager.handle(Event::Text('/'));
        for character in "changed_token_0002".chars() {
            pager.handle(Event::Text(character));
        }
        pager.handle(Event::Enter);
        let mut frame = Vec::with_capacity(16 * 1024);
        pager.draw(&mut frame).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn initial_regex_search_late_match(bencher: Bencher) {
    let text = corpus(20_000);
    bencher.bench_local(|| {
        let mut pager = session(&text);
        pager.handle(Event::Text('/'));
        for character in "changed_token_000[2]".chars() {
            pager.handle(Event::Text(character));
        }
        pager.handle(Event::Enter);
        let mut frame = Vec::with_capacity(16 * 1024);
        pager.draw(&mut frame).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn cached_search_redraw(bencher: Bencher) {
    let text = corpus(20_000);
    let mut pager = session(&text);
    pager.handle(Event::Text('/'));
    for character in "changed_token_0001".chars() {
        pager.handle(Event::Text(character));
    }
    pager.handle(Event::Enter);
    let mut frame = Vec::with_capacity(16 * 1024);
    pager.draw(&mut frame).unwrap();
    bencher.bench_local(|| {
        frame.clear();
        pager.draw(&mut frame).unwrap();
        black_box(frame.len())
    });
}

#[divan::bench]
fn live_search_chunk_and_redraw(bencher: Bencher) {
    let first = corpus(10_000);
    let second = "late changed_token_0002\n".repeat(100);
    bencher.bench_local(|| {
        let mut pager = Session::new(
            Size {
                rows: 24,
                columns: 100,
            },
            SessionOptions {
                title: "bench".into(),
                search_history: Vec::new(),
                wrap: false,
                follow: false,
                filter: None,
            },
        );
        pager.push_chunk(&first);
        pager.handle(Event::Text('/'));
        for character in "changed_token_0002".chars() {
            pager.handle(Event::Text(character));
        }
        pager.handle(Event::Enter);
        pager.push_chunk(&second);
        pager.advance();
        let mut frame = Vec::with_capacity(16 * 1024);
        pager.draw(&mut frame).unwrap();
        black_box(frame.len())
    });
}

fn main() {
    divan::main();
}
