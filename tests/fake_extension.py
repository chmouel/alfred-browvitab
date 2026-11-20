#!/usr/bin/env python3
"""A stand-in for the Vicinae browser extension, for tests and benchmarks.

Starts browvitab in native-host mode, exactly as a browser would, and
speaks the extension side of the protocol: browser/init, then
browser/tabs-changed, and it honours focus-tab / close-tab like the real
extension (closing re-sends the list).

    fake_extension.py BIN [--engine firefox|chrome] [--name Firefox]
                          [--count 50] [--pinned 2] [--seed 1]

Signals: SIGUSR1 adds a tab, SIGUSR2 renames the first tab.
Every message from the host is appended to $FAKE_EXT_LOG when set.
"""

import argparse
import json
import os
import random
import signal
import struct
import subprocess
import sys
import threading
import time

SITES = [
    ("GitHub – Pull Requests", "https://github.com/pulls"),
    ("Tekton Documentation", "https://tekton.dev/docs/"),
    ("Alfred 6 Documentation", "https://www.alfredapp.com/help/alfredui/"),
    ("Pipelines as Code", "https://github.com/openshift-pipelines/pipelines-as-code"),
    ("Reddit", "https://www.reddit.com/"),
    ("Gmail", "https://mail.google.com/mail/u/0/"),
    ("Rust std", "https://doc.rust-lang.org/std/"),
    ("Hacker News", "https://news.ycombinator.com/"),
    ("Kubernetes", "https://kubernetes.io/docs/home/"),
    ("YouTube", "https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bin")
    ap.add_argument("--engine", default="firefox")
    ap.add_argument("--name", default="Firefox")
    ap.add_argument("--count", type=int, default=50)
    ap.add_argument("--pinned", type=int, default=2)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--id-base", type=int, default=1)
    a = ap.parse_args()

    rnd = random.Random(a.seed)
    now = time.time() * 1000
    tabs = []
    for i in range(a.count):
        title, url = SITES[i % len(SITES)]
        tabs.append({
            "id": a.id_base + i,
            "title": f"{title} #{i}",
            "url": f"{url}?n={i}",
            "windowId": 1 + i % 3,
            "active": i == 0,
            "audible": i == 5,
            "muted": i == 6,
            "lastAccessed": now - rnd.randint(0, 86_400_000) if i else now,
            "pinned": i < a.pinned,
        })
    if a.engine == "chrome":
        argv = [a.bin, "chrome-extension://kcmipingpfbohfjckomimmahknoddnke/"]
    else:
        argv = [a.bin, "/fake/com.vicinae.vicinae.json", "firefox@vicinae.com"]
    host = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    lock = threading.Lock()
    log = os.environ.get("FAKE_EXT_LOG")

    def send(obj):
        body = json.dumps(obj).encode()
        with lock:
            host.stdin.write(struct.pack("=I", len(body)) + body)
            host.stdin.flush()

    def send_tabs():
        send({"method": "browser/tabs-changed", "data": tabs})

    def reader():
        while True:
            head = host.stdout.read(4)
            if len(head) < 4:
                return
            (n,) = struct.unpack("=I", head)
            msg = json.loads(host.stdout.read(n))
            if log:
                with open(log, "a") as f:
                    f.write(json.dumps(msg) + "\n")
            method = msg.get("method")
            if method and msg.get("id") is None:
                # Like extension 1.2: requests without an id are dropped.
                continue
            if method:
                send({"id": msg["id"], "result": None})
            tab_id = (msg.get("data") or {}).get("tabId")
            if method == "browser/close-tab":
                tabs[:] = [t for t in tabs if t["id"] != tab_id]
                send_tabs()
            elif method == "browser/focus-tab":
                for t in tabs:
                    t["active"] = t["id"] == tab_id
                    if t["active"]:
                        t["lastAccessed"] = time.time() * 1000
                send_tabs()

    threading.Thread(target=reader, daemon=True).start()
    send({"id": 1, "method": "browser/init", "data": {"name": a.name, "engine": a.engine}})
    send_tabs()

    def add(*_):
        n = max([t["id"] for t in tabs] + [a.id_base]) + 1
        tabs.append({"id": n, "title": f"New tab {n}", "url": f"https://example.org/new/{n}",
                     "windowId": 1, "active": False, "audible": False, "muted": False,
                     "lastAccessed": time.time() * 1000, "pinned": False})
        send_tabs()

    def rename(*_):
        if tabs:
            tabs[0]["title"] = f"Renamed {time.time():.0f}"
            send_tabs()

    signal.signal(signal.SIGUSR1, add)
    signal.signal(signal.SIGUSR2, rename)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    print(host.pid, flush=True)
    try:
        while host.poll() is None:
            time.sleep(0.2)
    finally:
        host.terminate()


if __name__ == "__main__":
    main()
