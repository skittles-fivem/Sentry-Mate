#!/usr/bin/env python3
"""Runs the tests of some packages the way `cargo test` does, but several test binaries at once.

    .github/run-tests.py [-j N] -- CARGO-TEST-ARGS...   (e.g. -- --locked -p web)

`cargo test` runs one test binary at a time, and most of them wait more than they work (a server started, files
written and read back), so a part of the fast set took as long as its slowest binaries added up (web: about 190 s
of tests on 4 cores). This builds every test binary once (`cargo test --no-run`, the same build), then runs them N
at a time (default: the machine's cores), each in its package's folder with CARGO_MANIFEST_DIR set, as cargo runs
them, and the doc tests beside them (`cargo test --doc`, only for the packages with a library). Each binary's
output is printed whole when it ends, with its time; the run fails when any of them does, and the times end the
log, slowest first (docs/development.md keeps what they were).
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor


def main() -> int:
    argv = sys.argv[1:]
    jobs = os.cpu_count() or 2
    if len(argv) >= 2 and argv[0] == "-j":
        jobs, argv = int(argv[1]), argv[2:]
    if argv[:1] == ["--"]:
        argv = argv[1:]
    started = time.monotonic()
    build = subprocess.run(
        ["cargo", "test", "--no-run", "--message-format=json-render-diagnostics", *argv],
        stdout=subprocess.PIPE,
        text=True,
    )
    if build.returncode != 0:
        return build.returncode
    tests = []  # (name, executable, package folder, package id)
    lib_packages = set()
    for line in build.stdout.splitlines():
        if not line.startswith("{"):
            continue
        m = json.loads(line)
        if m.get("reason") != "compiler-artifact":
            continue
        target = m["target"]
        package = m["package_id"]
        folder = os.path.dirname(m["manifest_path"])
        name = os.path.basename(folder)
        if "lib" in target["kind"] and not m["profile"]["test"]:
            lib_packages.add(name)
        if m.get("executable") and m["profile"]["test"]:
            kind = "unittests" if set(target["kind"]) & {"lib", "bin"} else "tests"
            tests.append((f"{name} {kind} {target['name']}", m["executable"], folder, package))
    print(f"built in {time.monotonic() - started:.0f} s: {len(tests)} test binaries, {jobs} at a time", flush=True)

    # The doc tests: only the asked packages that have a library (cargo test --doc refuses one without).
    asked = [a for i, a in enumerate(argv) if i > 0 and argv[i - 1] in ("-p", "--package")]
    doc_args = [a for a in argv if a.startswith("--locked") or a.startswith("--frozen") or a == "--offline"]
    if asked:
        doc_packages = [p for p in asked if p in lib_packages]
        doc = ["cargo", "test", "--doc", *doc_args] + [x for p in doc_packages for x in ("-p", p)] if doc_packages else None
    else:
        doc = ["cargo", "test", "--doc", *[a for a in argv if a != "--all-targets"]]

    def run(task):
        name, cmd, folder, env = task
        t = time.monotonic()
        with tempfile.TemporaryFile("w+") as out:
            code = subprocess.run(cmd, cwd=folder, env=env, stdout=out, stderr=subprocess.STDOUT).returncode
            out.seek(0)
            text = out.read()
        return name, code, time.monotonic() - t, text

    tasks = []
    for name, exe, folder, _ in sorted(tests):
        env = dict(os.environ, CARGO_MANIFEST_DIR=folder)
        tasks.append((name, [exe], folder, env))
    if doc:
        tasks.insert(0, ("doc tests", doc, os.getcwd(), dict(os.environ)))
    failed, times = [], []
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for name, code, secs, text in _as_done(pool, run, tasks):
            times.append((secs, name))
            print(f"::group::{name}: {'ok' if code == 0 else 'FAILED'} in {secs:.1f} s", flush=True)
            print(text, end="" if text.endswith("\n") else "\n")
            print("::endgroup::", flush=True)
            if code != 0:
                failed.append(name)
                print(f"::error::{name} failed (exit {code})", flush=True)
                # the output again, outside the folded group, so the failure is in sight
                print(text[-20000:], flush=True)
    print(f"all in {time.monotonic() - started:.0f} s; slowest:")
    for secs, name in sorted(times, reverse=True)[:12]:
        print(f"  {secs:6.1f} s  {name}")
    if failed:
        print("failed: " + ", ".join(failed))
        return 1
    return 0


def _as_done(pool, fn, tasks):
    from concurrent.futures import as_completed

    for f in as_completed([pool.submit(fn, t) for t in tasks]):
        yield f.result()


if __name__ == "__main__":
    sys.exit(main())
