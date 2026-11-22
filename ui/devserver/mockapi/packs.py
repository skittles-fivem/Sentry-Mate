"""UI packs as the real server has them (crates/web/src/packs): the built-in packs of ui/packs, each with the id
of its folder, plus the pack given with --pack when it is another folder. A person's `ui_pack` preference picks
the pack their browser is served, else the --pack one (the "chosen for everyone" of the dev server)."""

from __future__ import annotations

import json
import os

BUILTIN_DIR = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "packs"))
BUILTIN_ORDER = ["instrument", "logbook", "horizon", "console", "pebble", "manager"]
DEFAULT_PACK = "instrument"
API_VERSION = 1


def _meta(pid, folder, builtin):
    try:
        with open(os.path.join(folder, "ui.json"), encoding="utf-8") as f:
            m = json.load(f)
    except (OSError, ValueError):
        m = {}
    files, size = 0, 0
    for root, _dirs, names in os.walk(os.path.join(folder, "web")):
        for n in names:
            files += 1
            try:
                size += os.path.getsize(os.path.join(root, n))
            except OSError:
                pass
    api = m.get("api") if isinstance(m.get("api"), int) else 0
    return {"id": pid, "name": m.get("name") or pid, "description": m.get("description") or "",
            "author": m.get("author") or "", "version": m.get("version") or "", "api": api,
            "api_supported": api == API_VERSION, "builtin": builtin, "sha256": "0" * 64, "files": files,
            "bytes": size, "installed_ms": None, "folder": folder}


def pack_id(folder):
    return os.path.basename(os.path.normpath(folder))


class Packs:
    def __init__(self, served_folder):
        self.served = pack_id(served_folder)
        found = []
        for pid in BUILTIN_ORDER:
            d = os.path.join(BUILTIN_DIR, pid)
            if os.path.isdir(os.path.join(d, "web")):
                found.append(_meta(pid, d, True))
        if not any(p["id"] == self.served for p in found):
            found.append(_meta(self.served, os.path.normpath(served_folder), False))
        self.items = found

    def get(self, pid):
        return next((p for p in self.items if p["id"] == pid), None)

    def usable(self, pid):
        p = self.get(pid)
        return p is not None and p["api_supported"]

    def pick(self, own):
        """The pack a person is served: their own choice where it can be used, else the dev server's one."""
        return own if own and self.usable(own) else self.served

    def web_root(self, pid):
        return os.path.realpath(os.path.join(self.get(pid)["folder"], "web"))

    def listing(self):
        items = [{k: v for k, v in p.items() if k != "folder"} | {"chosen": p["id"] == self.served} for p in self.items]
        return {"items": items, "next": None, "chosen": self.served, "default": DEFAULT_PACK}
