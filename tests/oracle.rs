//! Differential oracle test: render every vendored input (git-show patches plus
//! git-log fixtures) with our library and compare byte-for-byte against the
//! golden oracle output (rendered by `/opt/homebrew/bin/delta` with the pinned
//! config, checked in under `oracle/` and `fixtures/oracle/`).
//!
//! The goldens are self-contained, so this test does not require delta to be
//! installed. Regenerate with `scripts/render-oracle.sh`.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_corpus(inputs: &PathBuf, goldens: &PathBuf) -> Vec<String> {
    let mut entries: Vec<_> = std::fs::read_dir(inputs)
        .expect("inputs dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "patch"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut failures = Vec::new();
    for entry in entries {
        let patch_path = entry.path();
        let base = patch_path.file_stem().unwrap().to_str().unwrap();
        let input = std::fs::read_to_string(&patch_path).unwrap();
        let golden = std::fs::read_to_string(goldens.join(format!("{base}.out"))).unwrap();
        if diff_pretty::render(&input) != golden {
            failures.push(base.to_string());
        }
    }
    failures
}

#[test]
fn oracle_byte_for_byte() {
    let mut failures = run_corpus(&root().join("patches"), &root().join("oracle"));
    let fixtures = root().join("fixtures");
    if fixtures.exists() {
        failures.extend(run_corpus(&fixtures, &fixtures.join("oracle")));
    }
    assert!(
        failures.is_empty(),
        "{} input(s) failed byte-for-byte: {}",
        failures.len(),
        failures.join(" ")
    );
}
