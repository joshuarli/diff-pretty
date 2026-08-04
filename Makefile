NAME       := diff-pretty
HOST       := $(shell rustc -vV | awk '/^host:/ {print $$2}')
TARGET     ?= $(subst -unknown-linux-gnu,-unknown-linux-musl,$(HOST))
MUSL_LOADER := $(if $(findstring x86_64,$(TARGET)),/lib/ld-musl-x86_64.so.1,/lib/ld-musl-aarch64.so.1)
MUSL_NATIVE_RUSTFLAGS := $(if $(findstring -linux-musl,$(TARGET)),-L native=/usr/lib)
TARGET_ENV := $(shell echo $(TARGET) | tr '[:lower:]-' '[:upper:]_')
MUSL_CRT_DIR := /usr/lib/diff-pretty-crt/$(TARGET)
LLVM_BIN   := $(shell rustc --print sysroot)/lib/rustlib/$(TARGET)/bin
PGO_DIR    := $(CURDIR)/target/pgo-profiles
PGO_MERGED := $(PGO_DIR)/merged.profdata

.PHONY: test check lint diff bench bench-diff release verify-release verify-release-dynamic release-pgo release-pgo-linux release-pgo-linux-static pgo-profile bench-pgo install

test:
	cargo test --release

check:
	@scripts/check.sh

lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- --deny warnings

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

test-ci:
	@test -x "target/$(TARGET)/release/$(NAME)"
	RUSTFLAGS="$(MUSL_NATIVE_RUSTFLAGS)" cargo test --quiet --release

release:
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="$(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

verify-release:
	@test -f "target/$(TARGET)/release/$(NAME)"
	@if otool -l "target/$(TARGET)/release/$(NAME)" 2>/dev/null | grep -q '__llvm_prf'; then \
		echo 'release still contains PGO profile sections; rebuild with make release-pgo' >&2; \
		exit 1; \
	fi
	@if echo "$(TARGET)" | grep -q -- '-linux-musl$$'; then \
		command -v readelf >/dev/null || { echo 'readelf is required for release verification'; exit 1; }; \
		file "target/$(TARGET)/release/$(NAME)" | grep -Eq 'static-pie linked|statically linked' || { echo 'release is not statically linked'; exit 1; }; \
		file "target/$(TARGET)/release/$(NAME)" | grep -q 'stripped' || { echo 'release is not stripped'; exit 1; }; \
		! readelf -l "target/$(TARGET)/release/$(NAME)" | grep -q INTERP || { echo 'release has a dynamic ELF interpreter'; exit 1; }; \
		! readelf -d "target/$(TARGET)/release/$(NAME)" | grep -q NEEDED || { echo 'release has dynamic dependencies'; exit 1; }; \
	else \
		echo "Skipping ELF checks for $(TARGET)"; \
	fi

verify-release-dynamic:
	@test -f "target/$(TARGET)/release/$(NAME)"
	@if echo "$(TARGET)" | grep -q -- '-linux-musl$$'; then \
		command -v readelf >/dev/null || { echo 'readelf is required for release verification'; exit 1; }; \
		file "target/$(TARGET)/release/$(NAME)" | grep -q 'dynamically linked' || { echo 'release is not dynamically linked'; exit 1; }; \
		file "target/$(TARGET)/release/$(NAME)" | grep -q 'stripped' || { echo 'release is not stripped'; exit 1; }; \
		readelf -l "target/$(TARGET)/release/$(NAME)" | grep -q '/lib/ld-musl-' || { echo 'release does not use the musl loader'; exit 1; }; \
		readelf -d "target/$(TARGET)/release/$(NAME)" | grep -q NEEDED || { echo 'release has no dynamic dependencies'; exit 1; }; \
	else \
		echo "Skipping ELF checks for $(TARGET)"; \
	fi

# Collect profiles from the representative user-facing workload.
# No build-std or -Cpanic=immediate-abort here: the profiler runtime needs unwinding.
pgo-profile:
	rm -rf $(PGO_DIR) && mkdir -p $(PGO_DIR)
	RUSTFLAGS="-Cprofile-generate=$(PGO_DIR)" \
	cargo bench --features bench-internals --bench bench -- render_pgo_training_workload --sample-size 8 --sample-count 24
	$(LLVM_BIN)/llvm-profdata merge -o $(PGO_MERGED) $(PGO_DIR)

# PGO-optimized release: uses gathered profiles + all aggressive flags.
release-pgo: pgo-profile
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED) -Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

release-pgo-linux: pgo-profile
	CARGO_TARGET_$(TARGET_ENV)_LINKER=clang \
	RUSTFLAGS="$(MUSL_NATIVE_RUSTFLAGS) -Cprofile-use=$(PGO_MERGED) -Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort -Ctarget-feature=-crt-static -Clink-arg=-B$(MUSL_CRT_DIR) -Clink-arg=-dynamic-linker=$(MUSL_LOADER)" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

release-pgo-linux-static: pgo-profile
	RUSTFLAGS="$(MUSL_NATIVE_RUSTFLAGS) -Cprofile-use=$(PGO_MERGED) -Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

# Benchmark regular release vs PGO and compare persisted baselines. Allocation
# count and volume are hard constraints: a faster PGO build that allocates more
# is a regression for this renderer.
bench-pgo: pgo-profile
	@BASELINE=$$(scripts/bench-baseline.py --print-path); \
	PGO_BASELINE=$$(scripts/bench-baseline.py --variant pgo --print-path); \
	scripts/bench-baseline.py --baseline "$$BASELINE" --quiet; \
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED)" \
	scripts/bench-baseline.py --baseline "$$PGO_BASELINE" --quiet --variant pgo; \
	scripts/diff-baselines.py "$$BASELINE" "$$PGO_BASELINE" \
	  --fail-on-allocation-regression --require-same-benchmarks

install: release-pgo verify-release
	cp target/$(TARGET)/release/$(NAME) ~/usr/bin/$(NAME)
	codesign -fs - ~/usr/bin/$(NAME)
