//! In-process wall-time benchmark: render every vendored patch and report
//! throughput. `cargo run --release --bin bench [iterations]`.
//!
//! This measures only the render function (no process startup), matching the
//! "in-process" wall-time requirement. Compare against the oracle by piping the
//! concatenated corpus through `/opt/homebrew/bin/delta` once (see
//! `scripts/bench.sh`).

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut patches: Vec<(String, String)> = std::fs::read_dir(root.join("patches"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "patch"))
        .map(|e| {
            let b = e.path().file_stem().unwrap().to_str().unwrap().to_string();
            let input = std::fs::read_to_string(e.path()).unwrap();
            (b, input)
        })
        .collect();
    patches.sort_by(|a, b| a.0.cmp(&b.0));

    // Warmup + total input/output size.
    let mut total_out = 0usize;
    for (_, input) in &patches {
        total_out += diff_pretty::render(input).len();
    }

    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iters {
        for (_, input) in &patches {
            checksum = checksum.wrapping_add(diff_pretty::render(input).len());
        }
    }
    let elapsed = start.elapsed();

    let input_bytes: usize = patches.iter().map(|(_, i)| i.len()).sum();
    println!("patches: {}", patches.len());
    println!("input bytes: {}", input_bytes);
    println!("output bytes (one pass): {}", total_out);
    println!("iterations: {}", iters);
    println!("total elapsed: {:.3?}", elapsed);
    println!(
        "per-call (whole corpus): {:.3} ms",
        elapsed.as_secs_f64() * 1000.0 / iters as f64
    );
    println!(
        "throughput: {:.1} MB/s (input), {:.1} MB/s (output)",
        (input_bytes as f64 * iters as f64) / elapsed.as_secs_f64() / 1e6,
        (total_out as f64 * iters as f64) / elapsed.as_secs_f64() / 1e6
    );
    eprintln!("checksum (sanity, non-zero): {}", checksum);
}
