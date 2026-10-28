#!/usr/bin/env python3
"""An HTTPS server and an HTTP one, for testing `net.fetch` over real sockets.

usage: https_server.py DIR TLS_PORT HTTP_PORT

Makes a test CA (DIR/ca.pem) and a certificate for `localhost` it signs, serves HTTPS on
TLS_PORT and plain HTTP on HTTP_PORT (both on 127.0.0.1), prints "ready" and serves until it
is killed. A GET answers `path=PATH host=HOST`, except /old, which redirects to /hello?x=1;
a POST answers `got BODY!` in chunks. Needs the openssl command."""
import http.server
import os
import ssl
import subprocess
import sys
import threading


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if self.path == "/old":
            self.send_response(302)
            self.send_header("Location", "/hello?x=1")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        body = ("path=%s host=%s" % (self.path, self.headers.get("Host", ""))).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.send_response(201)
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        for part in (b"got ", data, b"!"):
            self.wfile.write(b"%x\r\n%s\r\n" % (len(part), part))
        self.wfile.write(b"0\r\n\r\n")

    def log_message(self, *args):
        pass


def make_certs(d):
    def openssl(*args):
        subprocess.run(["openssl"] + list(args), cwd=d, check=True, capture_output=True)

    openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2",
            "-subj", "/CN=Jig Test CA")
    openssl("req", "-newkey", "rsa:2048", "-nodes", "-keyout", "srv.key", "-out", "srv.csr", "-subj", "/CN=localhost")
    with open(os.path.join(d, "ext.cnf"), "w") as f:
        f.write("subjectAltName=DNS:localhost\n")
    openssl("x509", "-req", "-in", "srv.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "srv.pem",
            "-days", "2", "-extfile", "ext.cnf")


def main():
    d, tls_port, http_port = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    make_certs(d)
    secure = http.server.ThreadingHTTPServer(("127.0.0.1", tls_port), Handler)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(os.path.join(d, "srv.pem"), os.path.join(d, "srv.key"))
    secure.socket = ctx.wrap_socket(secure.socket, server_side=True)
    plain = http.server.ThreadingHTTPServer(("127.0.0.1", http_port), Handler)
    threading.Thread(target=plain.serve_forever, daemon=True).start()
    print("ready", flush=True)
    secure.serve_forever()


if __name__ == "__main__":
    main()
