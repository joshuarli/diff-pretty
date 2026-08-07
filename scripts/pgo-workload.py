#!/usr/bin/env python3
"""Run the deterministic application workload used for LLVM PGO.

This is a process driver, not a benchmark target. It launches the explicit
diff-pretty binary and feeds it a checked-in patch through stdin, so compiler,
Cargo, rustybench, and benchmark-allocator work cannot enter the child profile.
"""

from __future__ import annotations

import argparse
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        required=True,
        help="explicit instrumented or release-shaped diff-pretty binary",
    )
    parser.add_argument(
        "--profile-dir",
        type=Path,
        help="directory for application .profraw files; enables profile mode",
    )
    parser.add_argument(
        "--iterations",
        type=int,
        default=1,
        help="number of child-process runs for a timing sample (default: 1)",
    )
    return parser.parse_args()


def run_once(binary: Path, fixture: Path, profile_dir: Path | None) -> int:
    environment = os.environ.copy()
    if profile_dir is not None:
        environment["LLVM_PROFILE_FILE"] = str(profile_dir / "diff-pretty-%p.profraw")

    started = time.perf_counter_ns()
    with fixture.open("rb") as input_stream:
        completed = subprocess.run(
            [str(binary), "--paging=never"],
            stdin=input_stream,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=environment,
            check=False,
        )
    elapsed = time.perf_counter_ns() - started
    if completed.returncode != 0:
        error = completed.stderr.decode(errors="replace").strip()
        raise RuntimeError(
            f"profile workload child failed with status {completed.returncode}"
            + (f": {error}" if error else "")
        )
    return elapsed


def main() -> int:
    args = parse_args()
    root = Path(__file__).resolve().parents[1]
    fixture = root / "fixtures" / "log_000.patch"

    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        raise SystemExit(f"profile workload binary is not executable: {args.binary}")
    if not fixture.is_file():
        raise SystemExit(f"profile workload fixture is missing: {fixture}")
    if args.profile_dir is not None:
        args.profile_dir.mkdir(parents=True, exist_ok=True)
    if args.iterations < 1:
        raise SystemExit("--iterations must be positive")

    samples = [run_once(args.binary, fixture, args.profile_dir) for _ in range(args.iterations)]
    if args.profile_dir is not None:
        raw_profiles = tuple(args.profile_dir.glob("*.profraw"))
        if not raw_profiles:
            raise SystemExit(
                f"application produced no raw profile in {args.profile_dir}"
            )
        print(f"profile workload: {len(raw_profiles)} application raw profile(s)")
    if args.iterations > 1:
        print(
            "profile workload timing: "
            f"median={statistics.median(samples) / 1_000_000:.3f} ms "
            f"min={min(samples) / 1_000_000:.3f} ms "
            f"max={max(samples) / 1_000_000:.3f} ms "
            f"n={len(samples)}"
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        print(f"pgo-workload: {error}", file=sys.stderr)
        raise SystemExit(1)
