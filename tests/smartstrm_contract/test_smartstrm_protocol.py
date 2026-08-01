from __future__ import annotations

import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen


MEDIA = bytes(range(100))


class FakeSmartStrmHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_HEAD(self) -> None:  # noqa: N802 - stdlib handler API
        self._send_media(head_only=True)

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler API
        if self.path == "/redirect/302":
            self.send_response(302)
            self.send_header("Location", "/media/movie.mkv")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self.path == "/redirect/307":
            self.send_response(307)
            self.send_header("Location", "/media/movie.mkv")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self._send_media(head_only=False)

    def _send_media(self, *, head_only: bool) -> None:
        if not self.path.startswith("/media/movie.mkv"):
            self.send_error(404)
            return

        requested = self.headers.get("Range")
        start, end = 0, len(MEDIA) - 1
        status = 200
        if requested:
            try:
                unit, value = requested.split("=", 1)
                left, right = value.split("-", 1)
                if unit != "bytes" or not left:
                    raise ValueError
                start = int(left)
                end = int(right) if right else end
                end = min(end, len(MEDIA) - 1)
                if start > end or start >= len(MEDIA):
                    self.send_response(416)
                    self.send_header("Content-Range", f"bytes */{len(MEDIA)}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                status = 206
            except ValueError:
                self.send_error(416)
                return

        body = MEDIA[start : end + 1]
        self.send_response(status)
        self.send_header("Content-Type", "video/x-matroska")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(len(body)))
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{len(MEDIA)}")
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        return


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *_args: object, **_kwargs: object):  # type: ignore[no-untyped-def]
        return None


class SmartStrmProtocolTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), FakeSmartStrmHandler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base_url = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join(timeout=2)

    def test_head_advertises_direct_play_capabilities(self) -> None:
        request = Request(f"{self.base_url}/media/movie.mkv", method="HEAD")
        with urlopen(request, timeout=2) as response:
            self.assertEqual(response.status, 200)
            self.assertEqual(response.headers["Accept-Ranges"], "bytes")
            self.assertEqual(response.headers["Content-Length"], str(len(MEDIA)))
            self.assertEqual(response.read(), b"")

    def test_redirects_expose_location_without_following_it(self) -> None:
        opener = build_opener(NoRedirect())
        for code in (302, 307):
            with self.subTest(code=code):
                request = Request(f"{self.base_url}/redirect/{code}")
                with self.assertRaises(HTTPError) as raised:
                    opener.open(request, timeout=2)
                self.assertEqual(raised.exception.code, code)
                self.assertEqual(raised.exception.headers["Location"], "/media/movie.mkv")

    def test_single_range_returns_partial_content(self) -> None:
        request = Request(f"{self.base_url}/media/movie.mkv")
        request.add_header("Range", "bytes=10-19")
        with urlopen(request, timeout=2) as response:
            self.assertEqual(response.status, 206)
            self.assertEqual(response.headers["Content-Range"], "bytes 10-19/100")
            self.assertEqual(response.read(), MEDIA[10:20])


if __name__ == "__main__":
    unittest.main()
