NAME       := diff-pretty
HOST       := $(shell rustc -vV | awk '/^host:/ {print $$2}')
TARGET     ?= $(subst -unknown-linux-gnu,-unknown-linux-musl,$(HOST))
MUSL_LOADER := $(if $(findstring x86_64,$(TARGET)),/lib/ld-musl-x86_64.so.1,/lib/ld-musl-aarch64.so.1)
MUSL_NATIVE_RUSTFLAGS := $(if $(findstring -linux-musl,$(TARGET)),-L native=/usr/lib)
TARGET_ENV := $(shell echo $(TARGET) | tr '[:lower:]-' '[:upper:]_')
MUSL_CRT_DIR := /usr/lib/diff-pretty-crt/$(TARGET)
LLVM_BIN   := $(shell rustc --print sysroot)/lib/rustlib/$(TARGET)/bin
PGO_VARIANT ?= native
PGO_TARGET_DIR = $(CURDIR)/target/pgo-instrument-$(PGO_VARIANT)
PGO_DIR = $(CURDIR)/target/pgo-profiles/$(TARGET)-$(PGO_VARIANT)
PGO_MERGED = $(PGO_DIR)/merged.profdata
PGO_BINARY = $(PGO_TARGET_DIR)/$(TARGET)/release/$(NAME)
PGO_PROFILE_FLAGS = -Cprofile-generate=$(PGO_DIR)
PGO_EXTRA_RUSTFLAGS ?= $(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options
PGO_TARGET_RUSTFLAGS = $(PGO_PROFILE_FLAGS) $(PGO_EXTRA_RUSTFLAGS)
PGO_TARGET_LINKER ?=
PGO_LINKER_ENV = $(if $(PGO_TARGET_LINKER),CARGO_TARGET_$(TARGET_ENV)_LINKER=$(PGO_TARGET_LINKER))
RELEASE_RUSTFLAGS := $(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort
RELEASE_EXTRA_RUSTFLAGS ?=
RELEASE_TARGET_RUSTFLAGS = $(RELEASE_RUSTFLAGS) $(RELEASE_EXTRA_RUSTFLAGS)
RELEASE_TARGET_LINKER ?=
RELEASE_LINKER_ENV = $(if $(RELEASE_TARGET_LINKER),CARGO_TARGET_$(TARGET_ENV)_LINKER=$(RELEASE_TARGET_LINKER))
PGO_USE_FLAGS = -Cprofile-use=$(PGO_MERGED) -Cllvm-args=-pgo-warn-missing-function

.PHONY: test check lint diff bench bench-diff release verify-release verify-release-dynamic \
	release-pgo release-pgo-linux release-pgo-linux-static pgo-instrument pgo-instrument-linux \
	pgo-instrument-linux-static pgo-profile \
	pgo-merge pgo-profile-linux pgo-profile-linux-static install

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
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(MUSL_NATIVE_RUSTFLAGS)" cargo test --quiet --release

release:
	cargo clean -p $(NAME) --release --target $(TARGET)
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
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
	@if strings "target/$(TARGET)/release/$(NAME)" 2>/dev/null | grep -q 'LLVM Profile'; then \
		echo 'release still contains the LLVM profile runtime; profile use must be limited to the application crate' >&2; \
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

# Build the actual release-shaped application binary with instrumentation. The
# target-scoped flags keep host build scripts, proc macros, tests, and benches
# outside the profile boundary. The instrument build deliberately omits
# build-std and panic=immediate-abort: on the host target, instrumenting the
# custom core/std build produces duplicate lang items, and the profile runtime
# needs the normal unwinding support used by this build.
pgo-instrument:
	rm -rf "$(PGO_TARGET_DIR)" "$(PGO_DIR)"
	mkdir -p "$(PGO_DIR)"
	$(PGO_LINKER_ENV) CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(PGO_TARGET_RUSTFLAGS)" \
	CARGO_TARGET_DIR="$(PGO_TARGET_DIR)" \
	cargo build --release --target "$(TARGET)" --bin "$(NAME)"
	@test -x "$(PGO_BINARY)"

# Run one deterministic, profile-only workload through the child application.
# The Python driver is deliberately outside Cargo so it cannot enter the LLVM
# profile, and it refuses to guess a binary path.
pgo-profile: pgo-instrument
	python3 scripts/pgo-workload.py \
	  --binary "$(PGO_BINARY)" \
	  --profile-dir "$(PGO_DIR)"
	$(MAKE) --no-print-directory pgo-merge PGO_VARIANT="$(PGO_VARIANT)" TARGET="$(TARGET)"

# Merge only raw profiles emitted by the explicitly launched application and
# retain an inspectable function report beside the merged profile.
pgo-merge:
	@test -n "$$(find "$(PGO_DIR)" -type f -name '*.profraw' -print -quit)" || { \
		echo 'no application raw profiles found; run make pgo-profile first' >&2; exit 1; }
	$(LLVM_BIN)/llvm-profdata merge -o "$(PGO_MERGED)" "$(PGO_DIR)"/*.profraw
	$(LLVM_BIN)/llvm-profdata show --all-functions --counts "$(PGO_MERGED)" > "$(PGO_DIR)/merged-functions.txt"
	@grep -q 'diff_pretty' "$(PGO_DIR)/merged-functions.txt" || { \
		echo 'merged profile has no diff-pretty application symbols' >&2; exit 1; }
	@if grep -Eq 'divan|_ZN5bench' "$(PGO_DIR)/merged-functions.txt"; then \
		echo 'merged profile contains benchmark symbols' >&2; exit 1; \
	fi

# Linux profile collection must happen inside the Dockerfile image and must
# match the final dynamic/static CRT and linker shape.
pgo-profile-linux: PGO_VARIANT = linux-dynamic
pgo-profile-linux: PGO_TARGET_LINKER = clang
pgo-profile-linux: PGO_EXTRA_RUSTFLAGS = $(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options -Ctarget-feature=-crt-static -Clink-arg=-B$(MUSL_CRT_DIR) -Clink-arg=-dynamic-linker=$(MUSL_LOADER)
pgo-profile-linux: pgo-profile

pgo-profile-linux-static: PGO_VARIANT = linux-static
pgo-profile-linux-static: PGO_EXTRA_RUSTFLAGS = $(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options
pgo-profile-linux-static: pgo-profile

pgo-instrument-linux: PGO_VARIANT = linux-dynamic
pgo-instrument-linux: PGO_TARGET_LINKER = clang
pgo-instrument-linux: PGO_EXTRA_RUSTFLAGS = $(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options -Ctarget-feature=-crt-static -Clink-arg=-B$(MUSL_CRT_DIR) -Clink-arg=-dynamic-linker=$(MUSL_LOADER)
pgo-instrument-linux: pgo-instrument

pgo-instrument-linux-static: PGO_VARIANT = linux-static
pgo-instrument-linux-static: PGO_EXTRA_RUSTFLAGS = $(MUSL_NATIVE_RUSTFLAGS) -Zlocation-detail=none -Zunstable-options
pgo-instrument-linux-static: pgo-instrument

# PGO-optimized release: build dependencies and build-std without profile
# runtime support, then apply the profile only to the application crate.
release-pgo: pgo-profile
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
	cargo rustc --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET) --bin $(NAME) -- \
	  $(PGO_USE_FLAGS)

release-pgo-linux: PGO_VARIANT = linux-dynamic
release-pgo-linux: RELEASE_TARGET_LINKER = clang
release-pgo-linux: RELEASE_EXTRA_RUSTFLAGS = -Ctarget-feature=-crt-static -Clink-arg=-B$(MUSL_CRT_DIR) -Clink-arg=-dynamic-linker=$(MUSL_LOADER)
release-pgo-linux: pgo-profile-linux
	$(RELEASE_LINKER_ENV) \
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)
	$(RELEASE_LINKER_ENV) \
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
	cargo rustc --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET) --bin $(NAME) -- \
	  $(PGO_USE_FLAGS)

release-pgo-linux-static: PGO_VARIANT = linux-static
release-pgo-linux-static: pgo-profile-linux-static
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)
	CARGO_TARGET_$(TARGET_ENV)_RUSTFLAGS="$(RELEASE_TARGET_RUSTFLAGS)" \
	cargo rustc --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET) --bin $(NAME) -- \
	  $(PGO_USE_FLAGS)

install: release-pgo verify-release
	cp target/$(TARGET)/release/$(NAME) ~/usr/bin/$(NAME)
	codesign -fs - ~/usr/bin/$(NAME)
