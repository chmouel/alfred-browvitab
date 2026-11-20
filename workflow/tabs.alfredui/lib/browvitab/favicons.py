"""Site icons from the browsers' own favicon databases, without any network
access. Databases are opened read-only with `immutable=1`, so a database the
browser holds locked never blocks. Icons are cached per host in
`~/Library/Caches/browvitab/favicons`; sites without one are retried weekly."""

from __future__ import annotations

import os
import re
import sqlite3
import time
from pathlib import Path
from urllib.parse import quote

from . import browsers

MISS_TTL = 7 * 24 * 3600
EXTENSIONS = ("png", "ico", "svg", "jpg", "gif", "webp")
# Bound the first-run cost; the rest are found on later openings.
MAX_LOOKUPS = 150

GECKO_SQL = """
SELECT i.data FROM moz_pages_w_icons p
JOIN moz_icons_to_pages ip ON ip.page_id = p.id
JOIN moz_icons i ON i.id = ip.icon_id
WHERE p.page_url LIKE ? OR p.page_url LIKE ?
ORDER BY abs(i.width - 32) LIMIT 1
"""
CHROMIUM_SQL = """
SELECT b.image_data FROM icon_mapping m
JOIN favicon_bitmaps b ON b.icon_id = m.icon_id
WHERE m.page_url LIKE ? OR m.page_url LIKE ?
ORDER BY abs(b.width - 32) LIMIT 1
"""


def cache_dir() -> Path | None:
    override = os.environ.get("BROWVITAB_CACHE_DIR")
    if override:
        return Path(override) / "favicons"
    home = os.environ.get("HOME")
    return Path(home) / "Library/Caches/browvitab/favicons" if home else None


def key(host: str) -> str:
    return re.sub(r"[^A-Za-z0-9.-]", "_", host)


def cached(directory: Path, host: str):
    """A path, False for a recent miss, or None when unknown."""
    k = key(host)
    for ext in EXTENSIONS:
        path = directory / f"{k}.{ext}"
        if path.is_file():
            return path
    try:
        if time.time() - (directory / f"{k}.none").stat().st_mtime < MISS_TTL:
            return False
    except OSError:
        pass
    return None


def _support() -> Path | None:
    home = os.environ.get("HOME")
    return Path(home) / "Library/Application Support" if home else None


def databases(tab) -> list:
    """(schema, path) favicon databases for a tab's browser, most specific first."""
    support = _support()
    browser = browsers.lookup(tab.browser_id)
    if not support or not browser:
        return []
    root = support / browser.root
    if browser.kind == "gecko":
        if tab.profile_dir:
            return [("gecko", Path(tab.profile_dir) / "favicons.sqlite")]
        return [
            ("gecko", p / "favicons.sqlite")
            for p in sorted((root / "Profiles").glob("*"))
            if (p / "favicons.sqlite").is_file()
        ]
    profiles = [p for p in sorted(root.glob("*")) if p.name == "Default" or p.name.startswith("Profile ")]
    profiles.sort(key=lambda p: p.name != "Default")
    return [("chromium", p / "Favicons") for p in profiles if (p / "Favicons").is_file()]


def sniff(data: bytes) -> str:
    if data.startswith(b"\x89PNG"):
        return "png"
    if data.startswith(b"\x00\x00\x01\x00"):
        return "ico"
    if data.startswith(b"\xff\xd8"):
        return "jpg"
    if data.startswith(b"GIF"):
        return "gif"
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "webp"
    head = data[:256].decode("utf-8", "replace").lower()
    return "svg" if "<svg" in head or head.startswith("<?xml") else "png"


def lookup(schema: str, db: Path, hosts: list, directory: Path) -> None:
    uri = "file:" + quote(str(db)) + "?immutable=1"
    try:
        conn = sqlite3.connect(uri, uri=True, timeout=0.5)
    except sqlite3.Error:
        return
    sql = GECKO_SQL if schema == "gecko" else CHROMIUM_SQL
    try:
        for host in hosts:
            try:
                row = conn.execute(sql, (f"https://{host}/%", f"http://{host}/%")).fetchone()
            except sqlite3.Error:
                return
            if row and row[0]:
                data = bytes(row[0])
                temp = directory / f".{key(host)}.{os.getpid()}.tmp"
                temp.write_bytes(data)
                os.replace(temp, directory / f"{key(host)}.{sniff(data)}")
    finally:
        conn.close()


def resolve(tabs: list) -> dict:
    """Icon paths by host for `tabs`, looking up hosts not cached yet."""
    icons: dict = {}
    directory = cache_dir()
    if not directory:
        return icons
    try:
        directory.mkdir(parents=True, exist_ok=True)
    except OSError:
        return icons
    wanted: dict = {}
    seen = set()
    lookups = 0
    for tab in tabs:
        if not tab.host or tab.host in seen:
            continue
        seen.add(tab.host)
        found = cached(directory, tab.host)
        if found:
            icons[tab.host] = found
        elif found is None and lookups < MAX_LOOKUPS:
            lookups += 1
            for db in databases(tab):
                wanted.setdefault(db, []).append(tab.host)
    if not wanted:
        return icons
    pending = {h for hosts in wanted.values() for h in hosts}
    for (schema, db), hosts in sorted(wanted.items(), key=lambda kv: str(kv[0][1])):
        hosts = [h for h in hosts if h in pending]
        if not hosts:
            continue
        lookup(schema, db, hosts, directory)
        for host in hosts:
            found = cached(directory, host)
            if found:
                pending.discard(host)
                icons[host] = found
    for host in pending:
        try:
            (directory / f"{key(host)}.none").write_bytes(b"")
        except OSError:
            pass
    return icons
