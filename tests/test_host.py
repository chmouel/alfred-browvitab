"""Tests for the native messaging host and its socket client."""

import io
import json
import os
import shutil
import socket
import struct
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "workflow/tabs.alfredui/lib"))

from browvitab import host  # noqa: E402


class Out(io.RawIOBase):
    def __init__(self):
        self.data = b""
        self.lock = threading.Lock()

    def writable(self):
        return True

    def write(self, b):
        with self.lock:
            self.data += bytes(b)
        return len(b)

    def messages(self):
        stream = io.BytesIO(self.data)
        out = []
        while True:
            body = host.read_frame(stream)
            if body is None:
                return out
            out.append(json.loads(body))


def frame(obj):
    return json.dumps(obj).encode()


def make_host(tabs):
    out = Out()
    h = host.Host({"id": "firefox", "name": "Firefox", "pid": 42}, out)
    h.handle_extension_message(frame({"method": "browser/tabs-changed", "data": tabs}))
    return h, out


def short_dir():
    # macOS socket paths must stay under 104 bytes.
    return Path(tempfile.mkdtemp(prefix="bvt", dir="/tmp"))


def serve(h, directory, max_clients=4):
    path = directory / f"{os.getpid()}.sock"
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(path))
    listener.listen(16)
    threading.Thread(target=host.serve_clients, args=(listener, h, max_clients), daemon=True).start()
    return path


class TestHost(unittest.TestCase):
    def test_invocation_detects_both_browsers(self):
        self.assertTrue(host.is_native_host_invocation(["x", "chrome-extension://abc/"]))
        self.assertTrue(host.is_native_host_invocation(["x", "/m.json", "firefox@vicinae.com"]))
        self.assertFalse(host.is_native_host_invocation(["x", "list"]))

    def test_frames_round_trip_and_reject_bad_sizes(self):
        buf = io.BytesIO()
        host.write_frame(buf, b'{"a":1}')
        self.assertEqual(host.read_frame(io.BytesIO(buf.getvalue())), b'{"a":1}')
        self.assertIsNone(host.read_frame(io.BytesIO(b"")))
        with self.assertRaises(host.HostError):
            host.read_frame(io.BytesIO(struct.pack("=I", 0)))

    def test_init_is_answered_with_the_same_id(self):
        h, out = make_host([])
        h.handle_extension_message(frame({"id": 7, "method": "browser/init", "data": {"name": "Firefox"}}))
        self.assertEqual(out.messages(), [{"id": 7, "result": {}}])

    def test_tabs_changed_replaces_the_listing_and_bumps_the_generation(self):
        h, _ = make_host([{"id": 1, "url": "https://a"}])
        before = h.generation
        h.handle_extension_message(
            frame({"method": "browser/tabs-changed", "data": [{"id": 2, "url": "https://b", "pinned": True}]})
        )
        answer = h.handle_request({"method": "list"})
        self.assertEqual([t["id"] for t in answer["listing"]["tabs"]], [2])
        self.assertTrue(answer["listing"]["tabs"][0]["pinned"])
        self.assertEqual(answer["generation"], before + 1)

    def test_focus_and_close_carry_an_id_and_reach_the_extension(self):
        h, out = make_host([{"id": 5, "url": "https://a"}])
        self.assertEqual(h.handle_request({"method": "focus", "tabId": 5}), {})
        self.assertEqual(h.handle_request({"method": "close", "tabId": 5}), {})
        self.assertEqual(
            out.messages(),
            [
                {"id": "browvitab-1", "method": "browser/focus-tab", "data": {"tabId": 5}},
                {"id": "browvitab-2", "method": "browser/close-tab", "data": {"tabId": 5}},
            ],
        )
        self.assertEqual(h.handle_request({"method": "list"})["listing"]["tabs"], [])
        self.assertIn("error", h.handle_request({"method": "focus", "tabId": 5}))

    def test_expired_requests_are_not_carried_out(self):
        h, out = make_host([{"id": 1, "url": "https://a"}])
        answer = h.handle_request({"method": "focus", "tabId": 1}, expires=time.monotonic() - 1)
        self.assertIn("error", answer)
        self.assertEqual(out.messages(), [])

    def test_watch_returns_on_change_or_timeout(self):
        h, _ = make_host([{"id": 1, "url": "https://a"}])
        since = h.generation
        start = time.monotonic()
        self.assertEqual(h.handle_request({"method": "watch", "since": since, "timeoutMs": 80})["generation"], since)
        self.assertGreaterEqual(time.monotonic() - start, 0.07)

        def later():
            time.sleep(0.05)
            h.handle_extension_message(frame({"method": "browser/tabs-changed", "data": []}))

        threading.Thread(target=later).start()
        start = time.monotonic()
        answer = h.handle_request({"method": "watch", "since": since, "timeoutMs": 5000})
        self.assertLess(time.monotonic() - start, 1)
        self.assertEqual(answer["generation"], since + 1)
        start = time.monotonic()
        h.handle_request({"method": "watch", "since": 0, "timeoutMs": 5000})
        self.assertLess(time.monotonic() - start, 0.1)

    def test_classifies_macos_bundles(self):
        c = lambda p: (host.classify_browser(p) or (None,))[0]  # noqa: E731
        self.assertEqual(c("/Applications/Firefox.app/Contents/MacOS/firefox"), "firefox")
        self.assertEqual(c("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"), "brave-browser")
        self.assertEqual(c("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"), "google-chrome")
        self.assertEqual(c("/Applications/LibreWolf.app/Contents/MacOS/librewolf"), "librewolf")
        self.assertIsNone(c("/bin/zsh"))


class TestSockets(unittest.TestCase):
    def setUp(self):
        self.dir = short_dir()

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def test_list_focus_and_watch_over_sockets(self):
        h, out = make_host([{"id": 9, "title": "t", "url": "https://t"}])
        path = serve(h, self.dir)
        browsers = host.list_browsers(self.dir, 2)
        self.assertEqual(len(browsers), 1)
        self.assertEqual(browsers[0]["listing"]["tabs"][0]["id"], 9)
        host.send_request(path, {"method": "focus", "tabId": 9})
        self.assertEqual(len(out.messages()), 1)
        with self.assertRaises(host.HostError):
            host.send_request(path, {"method": "focus", "tabId": 99})

        since = browsers[0]["generation"]

        def later():
            time.sleep(0.05)
            h.update_browser(lambda b: b.__setitem__("profile", "work"))

        threading.Thread(target=later).start()
        watched = host.ask_all(self.dir, 3, lambda _: {"method": "watch", "since": since, "timeoutMs": 2000})
        self.assertEqual(watched[0]["listing"]["browser"]["profile"], "work")

    def test_abandoned_watches_free_their_client_slot(self):
        h, _ = make_host([{"id": 1, "url": "https://a"}])
        path = serve(h, self.dir, max_clients=2)
        since = host.list_browsers(self.dir, 1)[0]["generation"]
        for _ in range(4):
            conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            conn.connect(str(path))
            conn.sendall(json.dumps({"method": "watch", "since": since, "timeoutMs": 30000}).encode() + b"\n")
            time.sleep(0.03)
            conn.close()
        time.sleep(host.WATCH_SLICE + 0.2)
        start = time.monotonic()
        self.assertEqual(len(host.list_browsers(self.dir, 1)), 1)
        self.assertLess(time.monotonic() - start, 0.5)

    def test_stale_sockets_are_removed(self):
        path = self.dir / "999999999.sock"
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.bind(str(path))
        s.close()
        self.assertTrue(path.exists())
        self.assertEqual(host.list_browsers(self.dir, 0.3), [])
        self.assertFalse(path.exists())


if __name__ == "__main__":
    unittest.main()
