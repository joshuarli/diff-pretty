#!/usr/bin/env python3
"""Minimal controlling-PTY regressions for the standalone scrl binary."""

import fcntl
import os
import pty
import select
import signal
import struct
import tempfile
import termios
import time


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(ROOT, "target", "debug", "scrl")
DIFF_BINARY = os.path.join(ROOT, "target", "debug", "diff-pretty")


def run_pager(contents, *arguments, rows=8):
    source = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", delete=False)
    source.write(contents)
    source.close()
    pid, master = pty.fork()
    if pid == 0:
        os.execv(BINARY, [BINARY, "--paging=always", *arguments, source.name])
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, 80, 0, 0))
    os.set_blocking(master, False)
    return pid, master, source.name


def run_diff_pager(contents, rows=50):
    source_read, source_write = os.pipe()
    pid, master = pty.fork()
    if pid == 0:
        os.dup2(source_read, 0)
        os.close(source_read)
        os.close(source_write)
        os.execv(DIFF_BINARY, [DIFF_BINARY])
    os.close(source_read)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, 80, 0, 0))
    os.write(source_write, contents.encode())
    os.close(source_write)
    os.set_blocking(master, False)
    return pid, master


def read_for(master, seconds=0.3):
    output = bytearray()
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        readable, _, _ = select.select([master], [], [], 0.02)
        if not readable:
            continue
        try:
            output.extend(os.read(master, 65536))
        except OSError:
            break
    return bytes(output)


def visible_status_width(frame):
    status = frame.split(b"\x1b[7m", 1)[1].split(b"\x1b[0m", 1)[0]
    return len(status.decode("utf-8"))


def stop(pid, master):
    try:
        os.write(master, b"q")
    except OSError:
        pass
    deadline = time.monotonic() + 0.3
    while time.monotonic() < deadline:
        waited, _ = os.waitpid(pid, os.WNOHANG)
        if waited:
            os.close(master)
            return
        read_for(master, 0.02)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    for _ in range(20):
        try:
            waited, _ = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            break
        if waited:
            break
        time.sleep(0.01)
    os.close(master)


def check_initial_status():
    pid, master, path = run_pager("\n".join(f"line {index}" for index in range(20)))
    try:
        output = read_for(master)
        first_frame = output.split(b"\x1b[H", 2)[-1]
        assert b"\xe2\x86\x91/\xe2\x86\x93 scroll" in first_frame, repr(first_frame)
        assert visible_status_width(first_frame) <= 80, repr(first_frame)
    finally:
        stop(pid, master)
        os.unlink(path)


def check_search_prompt():
    pid, master, path = run_pager("alpha\nbeta\ngamma\n")
    try:
        read_for(master)
        os.write(master, b"/")
        output = read_for(master)
        assert b"\x1b[7m /" in output, repr(output)
    finally:
        stop(pid, master)
        os.unlink(path)


def check_search_navigation_does_not_panic():
    pid, master, path = run_pager("binary\nnoise\n")
    try:
        read_for(master)
        os.write(master, b"/binary\r")
        output = read_for(master)
        os.write(master, b"n")
        output += read_for(master)
        assert b"panicked at" not in output, output.decode(errors="replace")
    finally:
        stop(pid, master)
        os.unlink(path)


def check_diff_pretty_default_opens_pager():
    lines = "".join(f" line {index}\n" for index in range(25))
    patch = (
        "diff --git a/file b/file\n"
        "--- a/file\n"
        "+++ b/file\n"
        "@@ -1,25 +1,25 @@\n"
        f"{lines}"
    )
    pid, master = run_diff_pager(patch)
    try:
        output = read_for(master)
        assert b"\x1b[?1049h" in output, repr(output)
    finally:
        stop(pid, master)


if __name__ == "__main__":
    if not os.path.isfile(BINARY) or not os.path.isfile(DIFF_BINARY):
        raise SystemExit(f"build first: {BINARY} and {DIFF_BINARY}")
    checks = [
        ("initial status", check_initial_status),
        ("search prompt", check_search_prompt),
        ("search navigation", check_search_navigation_does_not_panic),
        ("diff-pretty default pager", check_diff_pretty_default_opens_pager),
    ]
    failures = []
    for name, check in checks:
        try:
            check()
        except AssertionError as error:
            failures.append((name, str(error)))
            print(f"FAIL {name}: {error}")
        else:
            print(f"PASS {name}")
    if failures:
        raise SystemExit(1)
