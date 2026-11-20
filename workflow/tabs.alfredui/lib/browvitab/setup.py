"""The setup checklist shown until a browser connects."""

from __future__ import annotations

import platform
import subprocess
from pathlib import Path

from . import install
from .browsers import BROWSERS, Browser, app_path
from .host import CHROME_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME

STORE = {
    "gecko": "https://addons.mozilla.org/firefox/addon/vicinae/",
    "chromium": f"https://chromewebstore.google.com/detail/vicinae-integration/{CHROME_EXTENSION_ID}",
}


def has_extension(support: Path, browser: Browser) -> bool:
    root = support / browser.root
    if browser.kind == "gecko":
        for listing in (root / "Profiles").glob("*/extensions.json"):
            try:
                if FIREFOX_EXTENSION_ID in listing.read_text(errors="replace"):
                    return True
            except OSError:
                pass
        return False
    return any(
        (root / profile / "Extensions" / CHROME_EXTENSION_ID).is_dir()
        for profile in ["Default"] + [p.name for p in root.glob("Profile *")]
    )


def installed_browsers(support: Path) -> list:
    """Browsers that are installed and have been started at least once."""
    return [b for b in BROWSERS if (support / b.root).is_dir() and app_path(b)]


def owner_of(support: Path, browser: Browser):
    """The most relevant manifest owner among the browser's manifest directories."""
    owners = [install.manifest_owner(support / d / f"{HOST_NAME}.json") for d in browser.manifest_dirs]
    for kind in ("ours", "vicinae", "other"):
        for owner in owners:
            if owner and owner[0] == kind:
                return owner
    return None


def row(uid, symbol, colour, title, detail, action="", action_text="", takeover=""):
    entry = {
        "uid": uid,
        "sicon.image": {"type": "symbol", "src": symbol},
        "sicon.contenttintcolour": colour,
        "stitle.text": title,
        "sdetail.text": detail,
        "action": action,
        "takeover": takeover,
    }
    if action:
        entry.update({"sact.hidden": False, "sact.text": action_text})
    if takeover:
        entry["stakeover.hidden"] = False
    return entry


DONE = ("checkmark.circle.fill", "systemGreenColor")
TODO = ("circle.dashed", "secondaryLabelColor")
WAIT = ("clock", "systemOrangeColor")
WARN = ("exclamationmark.triangle.fill", "systemOrangeColor")


def rows(connected: list) -> list:
    """Checklist rows, in order. `action` is what ↩ or the row's button does."""
    support = install.support_dir()
    out = [row("python", *DONE, f"Python {platform.python_version()}", "Ready")]
    browsers = installed_browsers(support)
    if not browsers:
        out.append(
            row(
                "browsers",
                *WARN,
                "No supported browser found",
                "Install Firefox, Chrome or Brave, start it once, then come back",
            )
        )
        return out
    tabs_by_id: dict = {}
    for c in connected:
        bid = c["listing"]["browser"].get("id", "")
        tabs_by_id[bid] = tabs_by_id.get(bid, 0) + len(c["listing"].get("tabs", []))
    for b in browsers:
        uid = f"browser:{b.id}"
        owner = owner_of(support, b)
        if b.id in tabs_by_id:
            count = tabs_by_id[b.id]
            out.append(row(uid, *DONE, b.name, f"Connected, {count} tab{'' if count == 1 else 's'}"))
        elif not has_extension(support, b):
            out.append(
                row(uid, *TODO, b.name, "Add the Vicinae extension to this browser", f"store:{b.id}", "Get Extension")
            )
        elif owner and owner[0] == "vicinae":
            out.append(row(uid, *WARN, b.name, "Sends its tabs to Vicinae", takeover=f"takeover:{b.id}"))
        elif owner and owner[0] == "other":
            out.append(row(uid, *WARN, b.name, f"Talks to another program: {owner[1]}", takeover=f"takeover:{b.id}"))
        elif owner is None:
            out.append(row(uid, *TODO, b.name, "Extension installed, not connected yet", "connect", "Connect"))
        else:
            out.append(row(uid, *WAIT, b.name, "Waiting for the extension. Turn it off and on to connect now"))
    return out


def next_action(entries: list) -> str:
    """What ↩ does: connect first, then the first remaining step."""
    actions = [e["action"] for e in entries if e["action"]]
    return "connect" if "connect" in actions else (actions[0] if actions else "")


def run(action: str, launcher_root: Path) -> str:
    """Carry out a checklist action; returns a status line."""
    kind, _, browser_id = action.partition(":")
    browser = next((b for b in BROWSERS if b.id == browser_id), None)
    support = install.support_dir()
    if kind == "connect":
        launcher = install.install_files(support, launcher_root)
        reports = install.install_manifests(support, launcher)
        if any(r.outcome == "failed" for r in reports):
            return "Could not write every manifest; run browvitab install in Terminal for details"
        return "Connected. The extension picks it up within a minute, or turn it off and on"
    if kind == "store" and browser:
        url = STORE[browser.kind]
        app = app_path(browser)
        subprocess.run(["/usr/bin/open", "-a", str(app), url] if app else ["/usr/bin/open", url], check=False)
        return f"Opened the Vicinae extension page in {browser.name}"
    if kind == "takeover" and browser:
        launcher = install.install_files(support, launcher_root)
        install.install_manifests(support, launcher, only={browser.id})
        return f"{browser.name} now sends its tabs to Browvitab. Turn the extension off and on"
    return ""
