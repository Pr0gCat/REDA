"""Static file server for the viewer that never lets the browser cache.

`python -m http.server` sends no cache headers, so browsers apply their own
heuristics and hold on to `index.html` and the wasm bundle across reloads. That
turns "I rebuilt the compiler and the viewer looks identical" into a plausible
conclusion when the real story is that the page never reloaded -- a failure mode
that has already cost real debugging time here, because a stale viewer is
indistinguishable from a change that did nothing.

Every response gets `Cache-Control: no-store`, so a plain refresh always shows
the current build.
"""

import sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class NoCacheHandler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store, must-revalidate")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        super().end_headers()

    def log_message(self, fmt, *args):
        # SimpleHTTPRequestHandler logs every request to stderr, which buries
        # any real error in a wall of 200s. Keep only the failures.
        if args and isinstance(args[0], str) and " 200 " in args[0]:
            return
        super().log_message(fmt, *args)


def main() -> int:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    root = Path(__file__).resolve().parent
    handler = partial(NoCacheHandler, directory=str(root))
    with ThreadingHTTPServer(("127.0.0.1", port), handler) as httpd:
        print(f"serving {root} at http://localhost:{port}/ (no-store)", flush=True)
        httpd.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
