"""A very small PDF writer: pages of monospaced text lines (Courier), enough for a logbook export."""

from __future__ import annotations


def _esc(s):
    s = s.encode("latin-1", "replace").decode("latin-1")
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def text_pdf(title, lines, font_size=8, lines_per_page=70, landscape=True):
    w, h = (842, 595) if landscape else (595, 842)
    pages = [lines[i:i + lines_per_page] for i in range(0, max(1, len(lines)), lines_per_page)] or [[]]
    objs = []

    def add(b):
        objs.append(b if isinstance(b, bytes) else b.encode("latin-1"))
        return len(objs)

    font = add("<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>")
    pages_id = len(objs) + 1 + 2 * len(pages)  # placeholder index, filled below
    page_ids = []
    for n, chunk in enumerate(pages, 1):
        y = h - 40
        parts = [f"BT /F1 {font_size + 3} Tf 36 {y} Td ({_esc(title)}) Tj ET"]
        y -= 22
        for ln in chunk:
            parts.append(f"BT /F1 {font_size} Tf 36 {y} Td ({_esc(ln)}) Tj ET")
            y -= font_size + 2
        parts.append(f"BT /F1 {font_size} Tf 36 24 Td (Page {n} of {len(pages)}) Tj ET")
        stream = "\n".join(parts).encode("latin-1", "replace")
        content = add(b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"\nendstream")
        page_ids.append(add(f"<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {w} {h}] "
                            f"/Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R >>"))
    kids = " ".join(f"{p} 0 R" for p in page_ids)
    real_pages = add(f"<< /Type /Pages /Kids [{kids}] /Count {len(page_ids)} >>")
    assert real_pages == pages_id
    catalog = add(f"<< /Type /Catalog /Pages {real_pages} 0 R >>")
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for i, body in enumerate(objs, 1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode() + body + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root {catalog} 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)
