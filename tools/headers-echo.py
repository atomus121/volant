# tiny server that records the request headers it receives; used to prove that
# the browser's Cookie/Referer/User-Agent make it all the way to the wire.
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

LOG = sys.argv[2] if len(sys.argv) > 2 else "headers.log"
BODY = b"x" * (1 << 20)


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(self.requestline + "\n")
            for k, v in self.headers.items():
                f.write(f"  {k}: {v}\n")
        self.send_response(200)
        self.send_header("Content-Length", str(len(BODY)))
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Disposition", 'attachment; filename="hdr.bin"')
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, *a):
        pass


HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
