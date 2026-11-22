#!/usr/bin/env python3
"""Sentry Mate dev API server: serves a UI pack and /api/v1 on made-up data.

    python3 ui/devserver/serve.py --pack ui/packs/instrument --port 0

Prints exactly one line, "listening on http://HOST:PORT/", when it is ready. See README.md.
"""

from __future__ import annotations

import os
import signal
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from mockapi.config import parse_args  # noqa: E402
from mockapi.server import App  # noqa: E402


def main(argv=None):
    cfg = parse_args(argv)
    app = App(cfg)
    srv = app.make_server()
    host, port = srv.server_address[:2]
    print(f"listening on http://{host}:{port}/", flush=True)

    def stop(*_):
        app.events.shutdown()
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    try:
        srv.serve_forever(poll_interval=0.25)
    except KeyboardInterrupt:
        pass
    finally:
        app.events.shutdown()
        srv.server_close()


if __name__ == "__main__":
    main()
