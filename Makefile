# diff-pretty developer commands. Adapted from `~/d/e`'s Makefile: the `bench`
# target runs the curated divan suite (benches/bench.rs) through
# scripts/bench-baseline.py, which persists a per-host baseline and prints the
# delta vs the previous run.

.PHONY: test check diff bench bench-diff

test:
	cargo test --release

check:
	@scripts/check.sh

# Usage: scripts/diff.sh <FIXTURE_NAME>
diff:
	@scripts/diff.sh $(FIXTURE)

# Run the curated divan suite and persist a host baseline (see benches/bench.rs
# for what is measured and why).
bench:
	@scripts/bench-baseline.py

# Compare a candidate baseline against the persisted host baseline.
# Usage: make bench-diff AFTER=path/to/baseline.txt
bench-diff:
	@BASELINE=$$(scripts/bench-baseline.py --print-path); \
	test -f "$$BASELINE" || { echo "no baseline yet; run 'make bench' first" >&2; exit 1; }; \
	scripts/diff-baselines.py "$$BASELINE" "$(AFTER)"
