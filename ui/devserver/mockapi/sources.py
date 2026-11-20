"""Where each row and value came from (docs/ui-api.md, Conventions, "Sources").

Rows carry `sources` (a list of `{kind, site?, label}`, never empty), `source_label` (one line a page can show)
and the old one-word `source` (`can`, `teslamate`, `tesla_api`), kept for one release.

The made-up inputs: Comet (`c1`) has SMO and the Tesla API (most rows SMO, a fifth seen by both, a few by the API
alone, and an afternoon two days ago when SMO was offline and only the API saw the car); Maple (`c2`) has both on
every live row; Atlas (`c3`) has SMO and its TeslaMate import; the synthetic cars have SMO. The data itself is the
same as before; only the labels say which input gave it.
"""

from __future__ import annotations

from .util import DAY, h01

SITES = {"teslamate": "TeslaMate"}
SHORT = {"smo": "SMO", "tesla_api": "the Tesla API"}
LABEL = {"smo": "Data from SMO", "tesla_api": "Data from the Tesla API"}

# the words `source=` takes; `can` and `teslamate` are the old ones
FILTER_WORDS = ("smo", "tesla_api", "import", "import:teslamate", "all", "can", "teslamate")


def ref(kind, site=None):
    if kind == "import":
        return {"kind": "import", "site": site, "label": f"Imported from {SITES.get(site, site)}"}
    return {"kind": kind, "label": LABEL[kind]}


def label_of(refs):
    live = [SHORT[r["kind"]] for r in refs if r["kind"] != "import"]
    imported = [SITES.get(r["site"], r["site"]) for r in refs if r["kind"] == "import"]

    def joined(v):
        return v[0] if len(v) == 1 else ", ".join(v[:-1]) + " and " + v[-1]

    if live and imported:
        return f"Data from {joined(live)}; imported from {joined(imported)}"
    if live:
        return f"Data from {joined(live)}"
    if imported:
        return f"Imported from {joined(imported)}"
    return "No data yet"


def inputs_for(car, start_ms, now_day_index=None):
    """The live inputs of a live row of `car` starting at `start_ms` (stable for a seed and a time)."""
    cid = getattr(car, "id", car)
    if cid == "c2":
        return ["smo", "tesla_api"]
    if cid != "c1":
        return ["smo"]
    # Comet: SMO offline for an afternoon two days ago (12:00 to 18:00 UTC): the Tesla API alone saw the car
    day = int(start_ms // DAY)
    if now_day_index is not None and day == now_day_index - 2 and 12 <= (start_ms % DAY) // 3_600_000 < 18:
        return ["tesla_api"]
    x = h01("inputs", cid, int(start_ms // 60_000))
    if x < 0.2:
        return ["smo", "tesla_api"]
    if x < 0.25:
        return ["tesla_api"]
    return ["smo"]


def refs_of(source, inputs):
    if source == "teslamate":
        return [ref("import", "teslamate")]
    return [ref(i) for i in (inputs or ["smo"])]


def old_word(refs):
    kinds = [r["kind"] for r in refs]
    if "smo" in kinds:
        return "can"
    if "import" in kinds:
        return "teslamate"
    if "tesla_api" in kinds:
        return "tesla_api"
    return "can"


def fields(source, inputs=None):
    """`source`, `sources` and `source_label` for a row of `source` (`can`, `teslamate`) from `inputs`."""
    refs = refs_of(source, inputs)
    return {"source": old_word(refs), "sources": refs, "source_label": label_of(refs)}


def of_seg(s):
    """The fields for a dataset piece (a drive, charge, parked spell, gap or update)."""
    return fields(s.source, getattr(s, "inputs", None))


def keeps(word, source, inputs):
    """Whether a `source=` filter word keeps a row (`all` and None keep everything)."""
    if word in (None, "all"):
        return True
    if word in ("import", "import:teslamate", "teslamate"):
        return source == "teslamate"
    if source == "teslamate":
        return False
    ins = inputs or ["smo"]
    if word in ("smo", "can"):
        return "smo" in ins
    if word == "tesla_api":
        return "tesla_api" in ins
    return True


def car_refs(car_id, has_live, has_import):
    out = []
    if has_live:
        out += [ref(i) for i in (["smo", "tesla_api"] if car_id in ("c1", "c2") else ["smo"])]
    if has_import:
        out.append(ref("import", "teslamate"))
    return out


def value_ref(car_id, field=None):
    """The input a car's newest value of `field` came from: SMO, except what only the Tesla API sends on the cars
    that have it (the charge limit, the software version)."""
    if car_id in ("c1", "c2") and field in ("limit_pct", "version", "user_present", "keep_climate"):
        return ref("tesla_api")
    return ref("smo")


def canon(word):
    """A `source=` word in today's spelling: `can` is `smo`, `teslamate` and `import` are `import:teslamate`."""
    return {"can": "smo", "teslamate": "import:teslamate", "import": "import:teslamate"}.get(word, word)


def filter_refs(word):
    """The origins a filter word reads (`all`: every input and import)."""
    return {
        "all": [ref("smo"), ref("tesla_api"), ref("import", "teslamate")],
        "smo": [ref("smo")],
        "tesla_api": [ref("tesla_api")],
        "import:teslamate": [ref("import", "teslamate")],
    }[canon(word)]
