"""Tab ordering and ranking.

Without a query: pinned tabs of every browser first, then one section per
browser instance, the one used last on top. Within each list, tabs playing
sound come first, then the most recently used, and the tab the user is
looking at goes last, so the previous tab is the first row.

With a query the Alfred UI filters natively; `rank` only reorders so the best
matches lead and the sections no longer apply.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from .host import socket_pid


@dataclass
class TabView:
    uid: str  # "<host pid>:<tab id>": unique across browsers and profiles
    socket: Path
    tab_id: int
    title: str
    url: str
    host: str
    browser_id: str
    browser_name: str
    source: str  # "Firefox", or "Firefox · work" with several instances
    app: str | None
    profile_dir: str | None
    browser_pid: int | None
    pinned: bool
    audible: bool
    muted: bool
    current: bool
    last_accessed: float
    group: str


def url_host(url: str) -> str:
    """Lowercase host of an http(s) URL, without port; empty otherwise."""
    if "://" not in url:
        return ""
    scheme, rest = url.split("://", 1)
    if scheme.lower() not in ("http", "https"):
        return ""
    authority = re.split(r"[/?#]", rest, maxsplit=1)[0]
    authority = authority.rsplit("@", 1)[-1]
    if authority.startswith("["):
        host = authority.split("]", 1)[0] + "]"
    else:
        host = authority.split(":", 1)[0]
    return host.lower()


def display_host(host: str) -> str:
    return host[4:] if host.startswith("www.") else host


def _browser_label(browser: dict) -> str:
    return browser.get("name") or browser.get("id") or "Browser"


def _source_labels(browsers: list) -> dict:
    counts: dict = {}
    for c in browsers:
        bid = c["listing"]["browser"].get("id", "")
        counts[bid] = counts.get(bid, 0) + 1
    labels = {}
    for c in browsers:
        b = c["listing"]["browser"]
        name = _browser_label(b)
        if counts.get(b.get("id", ""), 0) > 1:
            if b.get("profile"):
                name = f"{name} · {b['profile']}"
            elif b.get("pid"):
                name = f"{name} · {b['pid']}"
        labels[str(c["socket"])] = name
    return labels


def order(browsers: list) -> list:
    """Order the tabs of connected browsers as described above."""
    pinned, sections = [], []
    for c in browsers:
        rows = [(c, t) for t in c["listing"].get("tabs", [])]
        pinned.extend(r for r in rows if r[1].get("pinned"))
        rest = [r for r in rows if not r[1].get("pinned")]
        if rest:
            sections.append(rest)
    everything = pinned + [r for s in sections for r in s]
    candidates = [r for r in everything if r[1].get("lastAccessed", 0) > 0]
    current = None
    if candidates:
        newest = max(candidates, key=lambda r: r[1]["lastAccessed"])
        if newest[1].get("active"):
            current = (str(newest[0]["socket"]), newest[1]["id"])

    def is_current(row):
        return current == (str(row[0]["socket"]), row[1]["id"])

    def by_use(row):
        return (is_current(row), not row[1].get("audible"), -row[1].get("lastAccessed", 0))

    pinned.sort(key=by_use)
    for s in sections:
        s.sort(key=by_use)
    sections.sort(key=lambda s: -max(r[1].get("lastAccessed", 0) for r in s))

    sources = _source_labels(browsers)

    def view(row, group):
        c, tab = row
        b = c["listing"]["browser"]
        pid = socket_pid(c["socket"])
        stem = str(pid) if pid is not None else Path(c["socket"]).stem
        title = tab.get("title", "")
        return TabView(
            uid=f"{stem}:{tab['id']}",
            socket=Path(c["socket"]),
            tab_id=tab["id"],
            title=title if title.strip() else tab.get("url", ""),
            url=tab.get("url", ""),
            host=url_host(tab.get("url", "")),
            browser_id=b.get("id", ""),
            browser_name=_browser_label(b),
            source=sources.get(str(c["socket"]), ""),
            app=b.get("app"),
            profile_dir=b.get("profileDir"),
            browser_pid=b.get("pid"),
            pinned=bool(tab.get("pinned")),
            audible=bool(tab.get("audible")),
            muted=bool(tab.get("muted")),
            current=is_current(row),
            last_accessed=tab.get("lastAccessed", 0),
            group=group,
        )

    out = [view(r, "Pinned") for r in pinned]
    for s in sections:
        label = sources.get(str(s[0][0]["socket"]), "")
        out.extend(view(r, label) for r in s)
    return out


def _url_words(url: str) -> str:
    rest = url.split("://", 1)[1] if "://" in url else url
    return " ".join(w for w in re.split(r"[^\w]|_", rest) if w)


def match_text(tab: TabView) -> str:
    """What the native filter matches: title, host, URL words and the full URL."""
    host = display_host(tab.host)
    parts = [tab.title, host]
    if tab.host and tab.host != host:
        parts.append(tab.host)
    parts += [_url_words(tab.url), tab.url]
    return " ".join(parts)


def _word_starts(haystack: str, needle: str) -> bool:
    start = True
    for i, ch in enumerate(haystack):
        if start and haystack.startswith(needle, i):
            return True
        start = not ch.isalnum()
    return False


def matches(tab: TabView, query: str) -> bool:
    """Approximates the dataview's "word match, any order" filter."""
    text = match_text(tab).lower()
    return all(_word_starts(text, word) for word in query.lower().split())


def _word_score(tab: TabView, title: str, url: str, word: str) -> int:
    host = display_host(tab.host)
    if title.startswith(word):
        return 100
    if host.startswith(word):
        return 90
    if _word_starts(title, word):
        return 70
    if _word_starts(host, word):
        return 60
    if _word_starts(url, word):
        return 35
    if word in title or word in url:
        return 10
    return 0


def rank(tabs: list, query: str) -> list:
    """Best matches first, ties kept in recency order. A pasted URL or
    domain puts the tabs of that site first."""
    query = query.strip().lower()
    if not query:
        return tabs
    words = query.split()
    pasted = url_host(query)
    if not pasted and len(words) == 1 and "." in query:
        pasted = url_host("https://" + query)
    scored = []
    for i, tab in enumerate(tabs):
        title, url = tab.title.lower(), tab.url.lower()
        score = sum(_word_score(tab, title, url, w) for w in words)
        if url == query:
            score += 1000
        if pasted and display_host(tab.host) == display_host(pasted):
            score += 500
        if tab.current:
            score = max(0, score - 5)
        scored.append((-score, i, tab))
    scored.sort(key=lambda s: (s[0], s[1]))
    return [s[2] for s in scored]
