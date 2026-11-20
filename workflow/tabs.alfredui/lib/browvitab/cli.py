"""Command line: the native messaging host (started by browsers) and the
back end for the Alfred UI and the Script Filter."""

from __future__ import annotations

import json
import os
import queue
import sys
import threading
import time
from pathlib import Path

from . import __version__
from . import host as hostmod

# Other modules are imported where needed: every Alfred action starts a new
# Python, and ctypes or sqlite3 would slow the common paths down.

LIST_TIMEOUT = 1.5
# How long one live-refresh watch waits before handing back to the pulse.
WATCH_WINDOW = 25.0
# This copy's root (holds bin/ and lib/).
ROOT = Path(__file__).resolve().parents[2]

USAGE = """\
browvitab: browser tabs for Alfred 6 (Vicinae extension host)

Usage:
  browvitab list                  Print connected browsers and tabs
  browvitab filter [QUERY]        Script Filter JSON
  browvitab focus UID             Switch to a tab and raise its window
  browvitab close UID             Close a tab
  browvitab url UID               Print a tab's URL
  browvitab install [--force]     Install the host and browser manifests
  browvitab uninstall             Remove what install added
  browvitab status                Show manifests and connected hosts
  browvitab setup-action ACTION   Run a setup checklist step
  browvitab version
Alfred UI back end:
  ui-load QUERY [FORCED] | ui-watch FINGERPRINT QUERY INSTANCE [FORCED]
  ui-rank QUERY [NAVIGATED] | ui-close UID [QUERY] | ui-enter UID SETUPMODE [FORCED]
  ui-setup-action ACTION [FORCED] | ui-quit [INSTANCE]
"""


class Fail(Exception):
    pass


def emit(value) -> None:
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def trace(args: list) -> None:
    """When /tmp/browvitab-<uid>/trace exists, log every invocation there
    (Alfred UI scripts have no visible stderr)."""
    path = Path(f"/tmp/browvitab-{os.getuid()}/trace")
    if path.exists():
        try:
            with open(path, "a") as f:
                f.write(f"{time.time():.3f} pid={os.getpid()} {json.dumps(args[1:])}\n")
        except OSError:
            pass


def socket_dir() -> Path:
    try:
        return hostmod.socket_dir()
    except OSError as error:
        raise Fail(f"socket directory: {error}")


def connected() -> list:
    return hostmod.list_browsers(socket_dir(), LIST_TIMEOUT)


def parse_uid(uid: str):
    """`<host pid>:<tab id>` to (socket, tab id). An unresolved `{value:…}`
    from the UI means nothing was selected."""
    if not uid or "{" in uid:
        raise Fail("no tab selected")
    try:
        pid, tab = uid.split(":", 1)
        return socket_dir() / f"{int(pid)}.sock", int(tab)
    except ValueError:
        raise Fail(f"invalid tab id {uid!r}")


# Commands for people.


def cmd_list(_args) -> None:
    from . import tabs

    browsers = connected()
    for c in browsers:
        b = c["listing"]["browser"]
        print(
            f"{b.get('name')} [{b.get('id')}] pid={b.get('pid')} profile={b.get('profile')} "
            f"app={b.get('app')} tabs={len(c['listing']['tabs'])} socket={c['socket']}"
        )
    for tab in tabs.order(browsers):
        print(f"{tab.uid:<10} {tab.group:<12} {tab.title}  ·  {tab.url}")


def cmd_filter(_args) -> None:
    from . import alfred, favicons, setup, tabs

    browsers = connected()
    ordered = tabs.order(browsers)
    checklist = None if browsers else setup.rows(browsers)
    emit(alfred.script_filter(ordered, favicons.resolve(ordered), checklist))


def cmd_focus(args) -> None:
    sock, tab_id = parse_uid(args[0] if args else "")
    browser = None
    try:
        answer = hostmod.request(sock, {"method": "list"}, time.monotonic() + LIST_TIMEOUT)
        browser = (answer.get("listing") or {}).get("browser")
    except (OSError, ValueError, hostmod.HostError):
        pass
    try:
        hostmod.send_request(sock, {"method": "focus", "tabId": tab_id})
    except hostmod.HostError as error:
        raise Fail(str(error))
    # The extension raises the window inside the browser; bring that browser
    # process (not just any instance of the app) to the front.
    pid = (browser or {}).get("pid")
    if pid:
        from . import macos

        for _ in range(4):
            if macos.frontmost_pid() == pid:
                break
            macos.activate_pid(pid)
            time.sleep(0.05)


def cmd_close(args) -> None:
    sock, tab_id = parse_uid(args[0] if args else "")
    try:
        hostmod.send_request(sock, {"method": "close", "tabId": tab_id})
    except hostmod.HostError as error:
        raise Fail(str(error))


def cmd_url(args) -> None:
    sock, tab_id = parse_uid(args[0] if args else "")
    answer = hostmod.request(sock, {"method": "list"}, time.monotonic() + LIST_TIMEOUT)
    for tab in (answer.get("listing") or {}).get("tabs", []):
        if tab.get("id") == tab_id:
            sys.stdout.write(tab.get("url", ""))
            return
    raise Fail("the tab is no longer open")


def _print_reports(reports) -> None:
    words = {"written": "installed", "unchanged": "already installed", "removed": "removed"}
    for r in reports:
        if r.outcome == "kept":
            what = f"left alone: {r.detail} (use --force to replace)"
        elif r.outcome == "failed":
            what = f"failed: {r.detail}"
        else:
            what = words[r.outcome]
        print(f"{', '.join(r.browsers)}: {r.path}: {what}")


def cmd_install(args) -> None:
    from . import install

    support = install.support_dir()
    launcher = install.install_files(support, ROOT)
    reports = install.install_manifests(support, launcher, "--force" in args)
    print(f"Host: {launcher}")
    if not reports:
        print("No supported browser found under ~/Library/Application Support.")
    _print_reports(reports)
    print("The extension reconnects on its own within a minute; toggle it off and on to connect now.")


def cmd_uninstall(_args) -> None:
    import shutil

    from . import favicons, install

    support = install.support_dir()
    reports = install.uninstall_manifests(support)
    if not reports:
        print("No manifest of ours to remove.")
    _print_reports(reports)
    target = install.install_dir(support)
    if target.exists():
        shutil.rmtree(target)
        print(f"Removed {target}")
    cache = favicons.cache_dir()
    if cache:
        shutil.rmtree(cache.parent, ignore_errors=True)


def cmd_status(_args) -> None:
    from . import install

    support = install.support_dir()
    for browsers, path, owner in install.status(support):
        if owner is None:
            who = "no manifest"
        elif owner[0] == "ours":
            who = f"browvitab at {owner[1]}" + ("" if os.path.exists(owner[1]) else " (missing!)")
        elif owner[0] == "vicinae":
            who = "Vicinae"
        else:
            who = f"other: {owner[1]}"
        print(f"{', '.join(browsers)}: {path}: {who}")
    print(f"Installed host version: {install.installed_version(support) or 'none'} (this copy {__version__})")
    browsers = connected()
    print(f"Connected hosts: {len(browsers)}")
    for c in browsers:
        b = c["listing"]["browser"]
        print(f"  {b.get('name')} (pid {b.get('pid')}, profile {b.get('profile')}): {len(c['listing']['tabs'])} tabs")


# Alfred UI back end.

TIP = "Tip: ⌃↩ closes a tab and keeps the list open, ⌘K shows more actions"


def show_tip_once() -> bool:
    from . import install

    try:
        marker = install.install_dir(install.support_dir()) / "tip-shown"
        if marker.exists() or not marker.parent.is_dir():
            return False
        marker.touch()
        return True
    except OSError:
        return False


def view(browsers: list, query: str, select, forced_setup: bool = False, status: str = ""):
    """JSON Actions for the window, and a fingerprint of what they show that
    ignores the query, so the watcher compares like with like."""
    from . import alfred, favicons, setup, tabs

    plain = tabs.order(browsers)
    icons = favicons.resolve(plain)
    data = [alfred.ui_entry(t, icons) for t in tabs.rank(plain, query)]
    checklist = setup.rows(browsers) if forced_setup or not browsers else None
    fingerprint = alfred.fingerprint([sorted(data, key=lambda e: e["uid"]), checklist])
    if not status:
        names: list = []
        for label in [t.source for t in plain] + [c["listing"]["browser"].get("name", "") for c in browsers]:
            if label and label not in names:
                names.append(label)
        profiled = [n for n in names if " · " in n]
        names = [n for n in names if " · " in n or not any(p.startswith(n + " · ") for p in profiled)]
        status = alfred.status_text(len(plain), names)
        if checklist is None and show_tip_once():
            status = TIP
    out = alfred.ui_update(data, status, select, "No matching tabs", checklist)
    out["variables"]["fingerprint"] = fingerprint
    return out, fingerprint


def refresh_installed_copy() -> None:
    """After a workflow update, refresh the copy the browsers start, but only
    once setup has been done."""
    from . import install

    try:
        support = install.support_dir()
        if install.installed_version(support) not in (None, __version__):
            install.install_files(support, ROOT)
    except OSError as error:
        hostmod.debug(f"cannot refresh the installed copy: {error}")


def ui_load(args) -> None:
    """Preload: fill the list before the window shows. The first row is the
    previous tab, so ↩ right away switches back."""
    import uuid

    from . import alfred, tabs

    refresh_installed_copy()
    clean_watcher_files()
    query = args[0] if args else ""
    browsers = connected()
    first = tabs.rank(tabs.order(browsers), query)
    out, _ = view(browsers, query, first[0].uid if first else alfred.FIRST, len(args) > 1 and args[1] == "1")
    out["variables"]["instance"] = uuid.uuid4().hex
    emit(out)


def ui_rank(args) -> None:
    """Reorder for the query once typing pauses; the native filter has
    already narrowed the list on every keystroke."""
    from . import alfred

    query = args[0] if args else ""
    navigated = len(args) > 1 and args[1] == "1"
    keep = navigated or not query.strip()
    emit(view(connected(), query, alfred.CURRENT if keep else alfred.FIRST)[0])


def watcher_file(instance: str) -> Path:
    """One pid file per open window: stacked windows each run a watcher."""
    name = "".join(ch for ch in instance if ch.isalnum()) or "default"
    return socket_dir().parent / f"watch-{name}.pid"


def clean_watcher_files() -> None:
    for path in socket_dir().parent.glob("watch-*.pid"):
        try:
            if time.time() - path.stat().st_mtime > 3600:
                path.unlink()
        except OSError:
            pass


def ui_watch(args) -> None:
    """The pulse. If what the window would show changed, print the update.
    Otherwise block until a host's generation moves, a browser connects or
    leaves, the checklist changes, the window closes, or 25 seconds pass, and
    exit without changes; the pulse restarts at once with fresh arguments."""
    from . import alfred, setup

    fingerprint, query, instance, forced = (list(args) + ["", "", "", ""])[:4]
    forced_setup = forced == "1"
    directory = socket_dir()
    me = str(os.getpid())
    pidfile = watcher_file(instance)
    pidfile.write_text(me)

    browsers = hostmod.list_browsers(directory, LIST_TIMEOUT)
    out, current = view(browsers, query, alfred.CURRENT, forced_setup)
    if current != fingerprint:
        emit(out)
        return
    checklist = setup.rows(browsers) if forced_setup or not browsers else None

    woke: queue.Queue = queue.Queue()
    for c in browsers:

        def wait(c=c):
            since = c["generation"]
            try:
                answer = hostmod.request(
                    c["socket"],
                    {"method": "watch", "since": since, "timeoutMs": int(WATCH_WINDOW * 1000)},
                    time.monotonic() + WATCH_WINDOW + 2,
                )
                if answer.get("generation") not in (None, since):
                    woke.put(True)
            except (OSError, ValueError, hostmod.HostError):
                pass  # a host that went away removes its socket; seen below

        threading.Thread(target=wait, daemon=True).start()

    sockets = hostmod.host_sockets(directory)
    started = time.monotonic()
    ticks = 0
    while True:
        try:
            woke.get(timeout=0.5)
            break
        except queue.Empty:
            ticks += 1
        try:
            ours = pidfile.read_text().strip() == me
        except OSError:
            ours = False
        if not ours or time.monotonic() - started >= WATCH_WINDOW or hostmod.host_sockets(directory) != sockets:
            break
        # The checklist also tracks files (extension installed, manifests).
        if checklist is not None and ticks % 4 == 0 and setup.rows(browsers) != checklist:
            break
    emit({})
    os._exit(0)  # drop the waiting threads


def ui_close(args) -> None:
    """Close and redraw at once, keeping the list open with the neighbouring
    row selected, so ⌃↩ can close several tabs in a row."""
    from . import alfred, tabs

    uid, query = (list(args) + ["", ""])[:2]
    directory = socket_dir()
    before = tabs.rank(tabs.order(hostmod.list_browsers(directory, LIST_TIMEOUT)), query)
    visible = [t.uid for t in before if tabs.matches(t, query)]
    neighbour = alfred.FIRST
    if uid in visible:
        i = visible.index(uid)
        rest = visible[i + 1 : i + 2] or visible[max(0, i - 1) : i]
        neighbour = rest[0] if rest else alfred.FIRST
    try:
        cmd_close([uid])
    except Fail as error:
        emit({"update": {"ids": {"status": {"text": f"Could not close: {error}"}}}, "sound": {"src": "Funk"}})
        return
    # The host drops the tab from its list as soon as it asks the browser.
    emit(view(hostmod.list_browsers(directory, LIST_TIMEOUT), query, neighbour)[0])


def ui_enter(args) -> None:
    """↩ in the search field: switch to the selected tab, or in the
    checklist, take the next step."""
    from . import alfred, setup

    uid, setup_mode, forced = (list(args) + ["", "", ""])[:3]
    has_tab = bool(uid) and "{" not in uid
    # A browser may have connected since the window last showed the checklist.
    if has_tab and (setup_mode != "1" or forced != "1"):
        emit({"finish": {"args": uid, "variables": {"tabaction": "focus"}}})
        return
    if setup_mode != "1":
        emit({})
        return
    browsers = connected()
    try:
        message = setup.run(setup.next_action(setup.rows(browsers)), ROOT)
    except OSError as error:
        message = f"Setup failed: {error}"
    out, _ = view(browsers, "", alfred.FIRST, forced == "1", message)
    out["pulse"] = {"now": True}
    emit(out)


def ui_setup_action(args) -> None:
    """A checklist button."""
    from . import alfred, setup

    action, forced = (list(args) + ["", ""])[:2]
    try:
        message = setup.run(action, ROOT)
    except OSError as error:
        message = f"Setup failed: {error}"
    out, _ = view(connected(), "", alfred.FIRST, forced == "1", message)
    out["pulse"] = {"now": True}
    emit(out)


def ui_quit(args) -> None:
    """The window is closing: its watcher exits within half a second once its
    pid file no longer names it."""
    try:
        watcher_file(args[0] if args else "").unlink()
    except OSError:
        pass
    emit({})


def cmd_setup_action(args) -> None:
    """Checklist step from the classic list; prints a line for a notification."""
    from . import setup

    print(setup.run(args[0] if args else "", ROOT))


COMMANDS = {
    "list": cmd_list,
    "filter": cmd_filter,
    "focus": cmd_focus,
    "close": cmd_close,
    "url": cmd_url,
    "install": cmd_install,
    "uninstall": cmd_uninstall,
    "status": cmd_status,
    "setup-action": cmd_setup_action,
    "ui-load": ui_load,
    "ui-watch": ui_watch,
    "ui-rank": ui_rank,
    "ui-close": ui_close,
    "ui-enter": ui_enter,
    "ui-setup-action": ui_setup_action,
    "ui-quit": ui_quit,
    "ui-debug": lambda _args: emit({}),  # traced no-op for inspecting UI state
    "version": lambda _args: print(__version__),
}


def main(argv: list) -> int:
    if hostmod.is_native_host_invocation(argv):
        return hostmod.run_native_host(argv)
    trace(argv)
    command = argv[1] if len(argv) > 1 else ""
    if command in ("", "-h", "--help", "help"):
        sys.stdout.write(USAGE)
        return 0
    handler = COMMANDS.get(command)
    if not handler:
        sys.stderr.write(f"browvitab: unknown command {command!r}\n\n{USAGE}")
        return 2
    try:
        handler(argv[2:])
    except Fail as error:
        sys.stderr.write(f"browvitab: {error}\n")
        return 1
    except BrokenPipeError:
        return 0
    return 0
