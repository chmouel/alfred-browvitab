"""Native messaging host for the Vicinae browser extension, and its client.

The browser starts this program as the `com.vicinae.vicinae` host when the
extension loads. The host keeps the tab list the extension reports
(`browser/tabs-changed`) and serves it over a Unix socket, one per running
browser instance. Clients ask the host to focus or close a tab, and the host
passes that on to the extension. A `generation` counter and a blocking
`watch` request let an open Alfred UI refresh on the extension's own events.

Client requests are one JSON line per connection:
`{"method": "list" | "focus" | "close" | "watch", ...}`, answered by one
JSON line `{"listing": ..., "generation": ..., "error": ...}`.
"""

from __future__ import annotations

import itertools
import json
import os
import select
import socket
import stat
import struct
import sys
import threading
import time
from pathlib import Path
from typing import Callable

from . import macos
from .browsers import BROWSERS, GECKO_IDS

HOST_NAME = "com.vicinae.vicinae"
CHROME_EXTENSION_ID = "kcmipingpfbohfjckomimmahknoddnke"
FIREFOX_EXTENSION_ID = "firefox@vicinae.com"

MAX_FRAME = 64 * 1024 * 1024
MAX_REQUEST = 64 * 1024
CLIENT_TIMEOUT = 2.0
# How long a client request waits for the browser to take a message.
OUTPUT_WAIT = 0.5
# Watchers hold a connection each, so leave room for them.
MAX_CLIENTS = 16
MAX_WATCH = 60.0
# How often a waiting watch checks whether its client is still there.
WATCH_SLICE = 0.25

TAB_DEFAULTS = {
    "id": 0,
    "title": "",
    "url": "",
    "windowId": 0,
    "active": False,
    "audible": False,
    "muted": False,
    "lastAccessed": 0.0,
    "pinned": False,
}


class HostError(Exception):
    pass


def debug(message: str) -> None:
    if os.environ.get("BROWVITAB_DEBUG"):
        sys.stderr.write(f"browvitab: {message}\n")


def monotonic_ms() -> int:
    """Milliseconds on the system-wide monotonic clock, shared by all processes."""
    return int(time.clock_gettime(time.CLOCK_MONOTONIC) * 1000)


def is_native_host_invocation(args: list) -> bool:
    """Chrome passes the extension origin first; Firefox passes the manifest
    path, then the extension id."""
    return (len(args) > 1 and args[1].startswith("chrome-extension://")) or (
        len(args) > 2 and args[2] == FIREFOX_EXTENSION_ID
    )


def _read_exact(stream, n: int) -> bytes:
    data = b""
    while len(data) < n:
        chunk = stream.read(n - len(data))
        if not chunk:
            break
        data += chunk
    return data


def read_frame(stream) -> bytes | None:
    """One native messaging frame (native-endian u32 length, then JSON), or
    None at end of input."""
    head = _read_exact(stream, 4)
    if not head:
        return None
    if len(head) < 4:
        raise HostError("truncated frame header")
    (size,) = struct.unpack("=I", head)
    if size == 0 or size > MAX_FRAME:
        raise HostError(f"invalid message size {size}")
    body = _read_exact(stream, size)
    if len(body) < size:
        raise HostError("truncated frame")
    return body


def write_frame(stream, body: bytes) -> None:
    stream.write(struct.pack("=I", len(body)) + body)
    stream.flush()


def normalize_tab(raw) -> dict | None:
    if not isinstance(raw, dict):
        return None
    tab = dict(TAB_DEFAULTS)
    for key, default in TAB_DEFAULTS.items():
        value = raw.get(key, default)
        if value is None:
            continue
        try:
            tab[key] = type(default)(value)
        except (TypeError, ValueError):
            pass
    return tab


class Host:
    """State shared by the extension reader and the socket clients."""

    def __init__(self, browser: dict, out, out_fd: int | None = None):
        self.cond = threading.Condition()
        self.listing = {"browser": dict(browser), "tabs": []}
        self.generation = 1
        self.out = out
        self.out_fd = out_fd
        self.out_lock = threading.Lock()
        self.ids = itertools.count(1)

    def mutate(self, change: Callable[[dict], None]) -> None:
        with self.cond:
            change(self.listing)
            self.generation += 1
            self.cond.notify_all()

    def update_browser(self, change: Callable[[dict], None]) -> None:
        self.mutate(lambda listing: change(listing["browser"]))

    def send(self, message: dict) -> None:
        body = json.dumps(message).encode()
        with self.out_lock:
            write_frame(self.out, body)

    def send_for_client(self, message: dict, expires: float | None) -> None:
        """Send for a client. When an earlier message is stuck because the
        browser stopped reading, give up instead of queueing behind it.
        Nothing is sent once `expires` (time.monotonic) has passed."""
        body = json.dumps(message).encode()
        give_up = time.monotonic() + OUTPUT_WAIT
        deadline = min(give_up, expires) if expires else give_up
        if not self.out_lock.acquire(timeout=max(0.0, deadline - time.monotonic())):
            raise HostError("the browser is not reading messages")
        try:
            if expires and time.monotonic() >= expires:
                raise HostError("the request expired")
            if self.out_fd is not None:
                left = max(0.0, deadline - time.monotonic())
                _, writable, _ = select.select([], [self.out_fd], [], left)
                if not writable:
                    raise HostError("the browser is not reading messages")
            write_frame(self.out, body)
        finally:
            self.out_lock.release()

    def handle_extension_message(self, raw: bytes) -> None:
        message = json.loads(raw)
        method = message.get("method")
        if not method:
            return  # a response to one of our requests
        msg_id = message.get("id")
        if method == "browser/init":
            name = (message.get("data") or {}).get("name")
            if isinstance(name, str) and name and "/" not in name:

                def rename(listing):
                    if listing["browser"].get("pid") is None:
                        listing["browser"]["name"] = name

                self.mutate(rename)
            if msg_id is not None:
                self.send({"id": msg_id, "result": {}})
        elif method == "browser/tabs-changed":
            data = message.get("data")
            if not isinstance(data, list):
                debug("invalid tab list")
                return
            tabs = [t for t in map(normalize_tab, data) if t]
            self.mutate(lambda listing: listing.__setitem__("tabs", tabs))
        elif msg_id is not None:
            self.send({"id": msg_id, "error": "unsupported method"})

    def snapshot(self) -> dict:
        return {
            "listing": json.loads(json.dumps(self.listing)),
            "generation": self.generation,
        }

    def handle_request(
        self,
        request: dict,
        expires: float | None = None,
        cancelled: Callable[[], bool] = lambda: False,
    ) -> dict:
        method = request.get("method")
        if method == "list":
            with self.cond:
                return self.snapshot()
        if method == "watch":
            since = request.get("since")
            wait = min(float(request.get("timeoutMs", 0)) / 1000.0, MAX_WATCH)
            end = time.monotonic() + wait
            if expires:
                end = min(end, expires)
            with self.cond:
                while self.generation == since:
                    left = end - time.monotonic()
                    if left <= 0 or cancelled():
                        break
                    self.cond.wait(min(left, WATCH_SLICE))
                return self.snapshot()
        if method not in ("focus", "close"):
            return {"error": f"unknown method {method!r}"}
        try:
            tab_id = int(request.get("tabId"))
        except (TypeError, ValueError):
            return {"error": "invalid tab id"}
        with self.cond:
            known = any(tab["id"] == tab_id for tab in self.listing["tabs"])
        if not known:
            return {"error": "the tab is no longer open"}
        # Extension 1.2 drops requests without an id.
        message = {
            "id": f"browvitab-{next(self.ids)}",
            "method": f"browser/{method}-tab",
            "data": {"tabId": tab_id},
        }
        try:
            self.send_for_client(message, expires)
        except (HostError, OSError) as error:
            return {"error": f"cannot reach the browser: {error}"}
        if method == "close":
            # Gone right away, so a list fetched before the extension's next
            # update does not show it.
            self.mutate(lambda listing: listing.__setitem__("tabs", [t for t in listing["tabs"] if t["id"] != tab_id]))
        return {}

    def serve_client(self, conn: socket.socket) -> None:
        conn.settimeout(CLIENT_TIMEOUT)
        line = b""
        while not line.endswith(b"\n") and len(line) < MAX_REQUEST:
            chunk = conn.recv(4096)
            if not chunk:
                break
            line += chunk
        try:
            request = json.loads(line)
            if not isinstance(request, dict):
                raise ValueError("not an object")
        except ValueError as error:
            response = {"error": f"invalid request: {error}"}
        else:
            expires = None
            if isinstance(request.get("expires"), (int, float)):
                left = (request["expires"] - monotonic_ms()) / 1000.0
                if left <= 0:
                    _send_line(conn, {"error": "the request expired"})
                    return
                expires = time.monotonic() + left
            response = self.handle_request(request, expires, lambda: peer_closed(conn))
        _send_line(conn, response)


def _send_line(conn: socket.socket, response: dict) -> None:
    try:
        conn.sendall(json.dumps(response).encode() + b"\n")
    except OSError:
        pass


def peer_closed(conn: socket.socket) -> bool:
    """Whether the client hung up: readable, with nothing to read. Clients
    send nothing after their request line."""
    try:
        readable, _, _ = select.select([conn], [], [], 0)
        if not readable:
            return False
        return conn.recv(1, socket.MSG_PEEK) == b""
    except OSError:
        return True


def serve_clients(listener: socket.socket, host: Host, max_clients: int) -> None:
    """Accept clients, one thread each, at most `max_clients` at a time."""
    slots = threading.BoundedSemaphore(max_clients)

    def run(conn):
        try:
            host.serve_client(conn)
        except OSError as error:
            debug(f"client error: {error}")
        finally:
            conn.close()
            slots.release()

    while True:
        try:
            conn, _ = listener.accept()
        except OSError:
            return
        if not slots.acquire(blocking=False):
            debug("too many clients, dropping one")
            conn.close()
            continue
        threading.Thread(target=run, args=(conn,), daemon=True).start()


def _private_dir(path: Path, uid: int) -> None:
    try:
        os.mkdir(path, 0o700)
    except FileExistsError:
        pass
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != uid:
        raise PermissionError(f"{path} is not a directory owned by you")
    if st.st_mode & 0o077:
        os.chmod(path, 0o700)


def socket_dir() -> Path:
    """`/tmp/browvitab-<uid>/browsers`: a fixed path that browsers (started by
    launchd) and Alfred scripts agree on, short enough for socket names."""
    uid = os.getuid()
    override = os.environ.get("BROWVITAB_SOCKET_DIR")
    if override:
        path = Path(override)
        _private_dir(path, uid)
        return path
    base = Path(f"/tmp/browvitab-{uid}")
    _private_dir(base, uid)
    path = base / "browsers"
    _private_dir(path, uid)
    return path


def classify_browser(exe: str):
    """(id, name) for a browser executable path, or None."""
    bundle = macos.app_bundle(exe)
    key = (Path(bundle).stem if bundle else Path(exe).name).lower()
    if key.startswith("firefox developer"):
        return ("firefox-developer", "Firefox Developer Edition")
    if key.startswith("firefox nightly"):
        return ("firefox-nightly", "Firefox Nightly")
    for browser in BROWSERS:
        if key in (browser.app.lower(), browser.id):
            return (browser.id, browser.name)
    # Variants such as "Zen Browser" or "Microsoft Edge Beta".
    for browser in BROWSERS:
        if key.startswith(browser.app.lower()):
            return (browser.id, browser.name)
    return None


def detect_browser(args: list) -> dict:
    """The browser among this process's ancestors, or a guess from the arguments."""
    pid: int | None = os.getppid()
    for _ in range(4):
        if not pid or pid <= 1:
            break
        exe = macos.exe_path(pid)
        found = classify_browser(exe) if exe else None
        if found:
            return {"id": found[0], "name": found[1], "pid": pid, "app": macos.app_bundle(exe)}
        pid = macos.parent_of(pid)
    if len(args) > 2 and args[2] == FIREFOX_EXTENSION_ID:
        return {"id": "firefox", "name": "Firefox", "pid": None}
    return {"id": "chromium", "name": "Chromium", "pid": None}


def run_native_host(args: list) -> int:
    browser = detect_browser(args)
    try:
        directory = socket_dir()
    except OSError as error:
        sys.stderr.write(f"browvitab host: no socket directory: {error}\n")
        return 1
    path = directory / f"{os.getpid()}.sock"
    try:
        path.unlink()
    except FileNotFoundError:
        pass
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(str(path))
        os.chmod(path, 0o600)
        listener.listen(64)
    except OSError as error:
        sys.stderr.write(f"browvitab host: cannot listen on {path}: {error}\n")
        return 1
    # Unbuffered, so each frame is one write the select in send_for_client
    # can vouch for.
    out_fd = os.dup(sys.stdout.fileno())
    out = os.fdopen(out_fd, "wb", buffering=0)
    host = Host(browser, out, out_fd)

    if browser.get("pid") and browser["id"] in GECKO_IDS:

        def find_profile(pid=browser["pid"]):
            directory_ = macos.gecko_profile_dir(pid)
            if directory_:
                name = macos.gecko_profile_name(directory_)

                def set_profile(b):
                    b["profile"] = name
                    b["profileDir"] = directory_

                host.update_browser(set_profile)

        threading.Thread(target=find_profile, daemon=True).start()

    threading.Thread(target=serve_clients, args=(listener, host, MAX_CLIENTS), daemon=True).start()

    stdin = sys.stdin.buffer
    code = 0
    try:
        while True:
            body = read_frame(stdin)
            if body is None:
                break
            try:
                host.handle_extension_message(body)
            except (ValueError, OSError) as error:
                debug(str(error))
    except (HostError, OSError) as error:
        sys.stderr.write(f"browvitab host: {error}\n")
        code = 1
    finally:
        try:
            path.unlink()
        except OSError:
            pass
    return code


# Client side.


def socket_pid(path: Path) -> int | None:
    try:
        return int(Path(path).stem)
    except ValueError:
        return None


def request(path: Path, req: dict, deadline: float) -> dict:
    """Send one request and read the answer, all before `deadline`
    (time.monotonic). The host drops requests that reach it late."""
    while True:
        left = deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError("the browser host is not accepting connections")
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        conn.settimeout(left)
        try:
            conn.connect(str(path))
            break
        except ConnectionRefusedError:
            conn.close()
            pid = socket_pid(path)
            # macOS refuses at once when a live host's backlog is full.
            if not (pid and macos.alive(pid)):
                raise
            time.sleep(0.01)
        except OSError:
            conn.close()
            raise
    with conn:
        left = deadline - time.monotonic()
        message = dict(req, expires=monotonic_ms() + int(left * 1000))
        conn.sendall(json.dumps(message).encode() + b"\n")
        answer = b""
        while not answer.endswith(b"\n"):
            left = deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError("the browser host did not answer in time")
            conn.settimeout(left)
            try:
                chunk = conn.recv(65536)
            except socket.timeout:
                raise TimeoutError("the browser host did not answer in time")
            if not chunk:
                break
            answer += chunk
            if len(answer) > MAX_FRAME:
                raise HostError("answer too large")
    return json.loads(answer)


def send_request(path: Path, req: dict) -> None:
    """Ask a host to focus or close a tab; raises HostError on failure."""
    try:
        response = request(path, req, time.monotonic() + CLIENT_TIMEOUT)
    except (FileNotFoundError, ConnectionRefusedError):
        raise HostError("the browser is no longer connected")
    except (OSError, ValueError) as error:
        raise HostError(f"browser host: {error}")
    if response.get("error"):
        raise HostError(response["error"])


def host_sockets(directory: Path) -> list:
    uid = os.getuid()
    found = []
    try:
        entries = list(os.scandir(directory))
    except OSError:
        return []
    for entry in entries:
        if not entry.name.endswith(".sock"):
            continue
        try:
            st = entry.stat(follow_symlinks=False)
        except OSError:
            continue
        if stat.S_ISSOCK(st.st_mode) and st.st_uid == uid:
            found.append(Path(entry.path))
    return sorted(found)


def ask_all(directory: Path, timeout: float, make_request: Callable[[Path], dict]) -> list:
    """Ask every host at once, waiting at most `timeout` overall. Returns
    [{"socket", "listing", "generation"}]. Sockets whose host is gone are removed."""
    sockets = host_sockets(directory)
    deadline = time.monotonic() + timeout
    answers: list = [None] * len(sockets)

    def ask(i, path):
        try:
            answers[i] = request(path, make_request(path), deadline)
        except (OSError, ValueError, HostError) as error:
            answers[i] = error

    threads = [threading.Thread(target=ask, args=(i, p), daemon=True) for i, p in enumerate(sockets)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(max(0.0, deadline - time.monotonic()) + 0.05)
    connected = []
    for path, answer in zip(sockets, answers):
        if isinstance(answer, dict) and isinstance(answer.get("listing"), dict):
            connected.append(
                {
                    "socket": path,
                    "listing": answer["listing"],
                    "generation": answer.get("generation", 0),
                }
            )
            continue
        if isinstance(answer, (ConnectionRefusedError, FileNotFoundError)):
            pid = socket_pid(path)
            if not (pid and macos.alive(pid)):
                try:
                    path.unlink()
                except OSError:
                    pass
        debug(f"{path}: {answer}")
    return connected


def list_browsers(directory: Path, timeout: float = 1.5) -> list:
    return ask_all(directory, timeout, lambda _: {"method": "list"})
