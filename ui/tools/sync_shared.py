#!/usr/bin/env python3
"""Copy the shared browser modules into every UI pack.

The canonical modules live in ui/shared/*.js. Each pack serves its own copy from
ui/packs/<name>/web/shared/ (instrument, logbook, horizon, console, pebble, and any new pack folder
with ui.json and web/), so a pack folder (or its zip) works on its own. This tool writes those copies;
nothing else should edit them.

    python3 ui/tools/sync_shared.py               copy into every pack
    python3 ui/tools/sync_shared.py --pack NAME   copy into one pack
    python3 ui/tools/sync_shared.py --check       change nothing; exit 1 if a copy differs

Only the top-level *.js files of ui/shared are copied (not its tests, README or package.json).
A .js file in a pack's web/shared/ that no longer exists in ui/shared is removed. Python
standard library only.
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
from pathlib import Path

UI_DIR = Path(__file__).resolve().parent.parent
SHARED_DIR = UI_DIR / "shared"
PACKS_DIR = UI_DIR / "packs"


def shared_files() -> dict[str, bytes]:
    """name -> content of every module to copy."""
    files = {}
    for p in sorted(SHARED_DIR.glob("*.js")):
        if p.is_file():
            files[p.name] = p.read_bytes()
    return files


def packs() -> list[Path]:
    """Every pack folder: a folder in ui/packs with ui.json and web/."""
    return sorted(p for p in PACKS_DIR.iterdir() if p.is_dir() and (p / "ui.json").is_file() and (p / "web").is_dir())


def write_atomic(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix="." + path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise


def differences(pack: Path, files: dict[str, bytes]) -> list[tuple[str, str]]:
    """[(kind, name)] where kind is 'missing', 'differs' or 'extra'."""
    dest = pack / "web" / "shared"
    out = []
    for name, data in files.items():
        target = dest / name
        if not target.is_file():
            out.append(("missing", name))
        elif target.read_bytes() != data:
            out.append(("differs", name))
    if dest.is_dir():
        for p in sorted(dest.glob("*.js")):
            if p.name not in files:
                out.append(("extra", p.name))
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Copy ui/shared/*.js into each pack's web/shared/.")
    ap.add_argument("--pack", help="only this pack (a folder name in ui/packs)")
    ap.add_argument("--check", action="store_true", help="change nothing; exit 1 if any copy is missing, differs or is extra")
    args = ap.parse_args(argv)

    files = shared_files()
    if not files:
        print(f"no modules found in {SHARED_DIR}", file=sys.stderr)
        return 2

    all_packs = packs()
    if args.pack:
        chosen = [p for p in all_packs if p.name == args.pack]
        if not chosen:
            names = ", ".join(p.name for p in all_packs) or "none"
            print(f"no pack named {args.pack!r} (packs: {names})", file=sys.stderr)
            return 2
    else:
        chosen = all_packs
    if not chosen:
        print(f"no packs found in {PACKS_DIR}", file=sys.stderr)
        return 2

    rel = lambda p: p.relative_to(UI_DIR.parent).as_posix()  # noqa: E731
    problems = 0
    for pack in chosen:
        diffs = differences(pack, files)
        dest = pack / "web" / "shared"
        if args.check:
            for kind, name in diffs:
                print(f"{kind}: {rel(dest / name)}")
            problems += len(diffs)
            continue
        for kind, name in diffs:
            target = dest / name
            if kind == "extra":
                target.unlink()
            else:
                write_atomic(target, files[name])
        changed = len(diffs)
        print(f"{pack.name}: {len(files)} modules, {changed} changed" if changed else f"{pack.name}: {len(files)} modules, up to date")

    if args.check:
        if problems:
            print(f"{problems} file(s) out of date; run python3 ui/tools/sync_shared.py", file=sys.stderr)
            return 1
        print(f"all copies match ({len(chosen)} pack(s), {len(files)} modules)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
