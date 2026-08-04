//! Golden snapshot regression test.
//!
//! Render every vendored fixture and compare byte-for-byte against the checked-in
//! goldens under `fixtures/oracle/`. The goldens are a **frozen baseline**
//! originally produced by delta (the reference implementation) — they are what
//! the implementation is expected to match.
//!
//! Inputs: `fixtures/*.patch` — `show_*` = `git show` (100 commits), `log_*` =
//! `git log -p` (plain + colorized), `plain_unified.patch` = `diff -u`.
//!
//! This is intentionally independent of delta: it reads only the checked-in
//! goldens. As the implementation intentionally diverges, update the matching
//! golden(s) in `fixtures/oracle/` to reflect the new expected output.

use std::path::PathBuf;

#[test]
fn golden_snapshot() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let goldens = fixtures.join("oracle");

    let mut entries: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("fixtures/ dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "patch"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    assert!(
        !entries.is_empty(),
        "no fixtures under {}",
        fixtures.display()
    );

    let mut failures = Vec::new();
    for entry in entries {
        let file = entry.path();
        let base = file.file_stem().unwrap().to_str().unwrap();
        let input = std::fs::read_to_string(&file).unwrap();
        let golden = std::fs::read_to_string(goldens.join(format!("{base}.out"))).unwrap();
        let rendered = diff_pretty::render(&input);
        let document = diff_pretty::render_document(&input);
        let mut retained = Vec::with_capacity(document.len());
        document.write_to(&mut retained).unwrap();
        let mut streamed = Vec::with_capacity(golden.len());
        diff_pretty::render_to(&input, &mut streamed).unwrap();
        let mut incremental = Vec::with_capacity(golden.len());
        diff_pretty::render_reader_to(input.as_bytes(), &mut incremental).unwrap();
        let incremental_document = diff_pretty::render_reader_document(input.as_bytes()).unwrap();
        let mut incremental_retained = Vec::with_capacity(incremental_document.len());
        incremental_document
            .write_to(&mut incremental_retained)
            .unwrap();
        if rendered != golden
            || retained != golden.as_bytes()
            || streamed != golden.as_bytes()
            || incremental != golden.as_bytes()
            || incremental_retained != golden.as_bytes()
        {
            failures.push(base.to_string());
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) differ from the golden baseline; update fixtures/oracle/ if the change is intentional: {}",
        failures.len(),
        failures.join(" ")
    );
}
