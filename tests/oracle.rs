//! Differential oracle test: render every vendored fixture with our library and
//! compare byte-for-byte against the golden oracle output (rendered by
//! `/opt/homebrew/bin/delta` with the pinned config, checked in under
//! `fixtures/oracle/`).
//!
//! Inputs: `fixtures/*.patch` — `show_*` = `git show` (100 commits), `log_*` =
//! `git log -p` (plain + colorized), `plain_unified.patch` = `diff -u`.
//!
//! The goldens are self-contained, so this test does not require delta to be
//! installed. Regenerate with `scripts/render-oracle.sh`.

use std::path::PathBuf;

#[test]
fn oracle_byte_for_byte() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let goldens = fixtures.join("oracle");

    let mut entries: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("fixtures/ dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "patch"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    assert!(!entries.is_empty(), "no fixtures under {}", fixtures.display());

    let mut failures = Vec::new();
    for entry in entries {
        let file = entry.path();
        let base = file.file_stem().unwrap().to_str().unwrap();
        let input = std::fs::read_to_string(&file).unwrap();
        let golden = std::fs::read_to_string(goldens.join(format!("{base}.out"))).unwrap();
        if diff_pretty::render(&input) != golden {
            failures.push(base.to_string());
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) failed byte-for-byte: {}",
        failures.len(),
        failures.join(" ")
    );
}
