"""Static server for the Tacit web app in development.

Makes the page cross-origin isolated, which browsers require before they
allow SharedArrayBuffer (and so WebAssembly threads). It also answers a
request for a wasm-bindgen package folder with the package's entry script:
wasm-bindgen-rayon's thread helpers import the package by its folder, which a
bundler would resolve and a plain static server would not.
"""
import http.server
import os
import sys


class Isolated(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".js": "text/javascript",
        ".wasm": "application/wasm",
    }

    def translate_path(self, path):
        local = super().translate_path(path)
        entry = os.path.join(local, "tacit_wasm.js")
        if os.path.isdir(local) and os.path.isfile(entry):
            return entry
        return local

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.abspath(__file__)))
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8770
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Isolated).serve_forever()
