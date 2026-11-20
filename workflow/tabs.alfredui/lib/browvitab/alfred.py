"""Output for Alfred: dataview data and JSON Actions for the Alfred UI, and
Script Filter JSON for the classic result list."""

from __future__ import annotations

import hashlib
import json
import re

from .tabs import TabView, display_host, match_text

_PLACEHOLDER = re.compile(r"\{(?=(var|const|file|fileraw|value|data|param):)", re.IGNORECASE)


def defuse(text: str) -> str:
    """Alfred UI expands `{var:…}`, `{file:…}` and friends in JSON it is given,
    including script output. Page titles are untrusted, so break up anything
    that looks like a placeholder with a zero-width space."""
    return _PLACEHOLDER.sub("{\u200b", text) if "{" in text else text


def subtitle(tab: TabView) -> str:
    parts = []
    host = display_host(tab.host)
    if host:
        parts.append(host)
    elif tab.url:
        parts.append(re.split(r"[?#]", tab.url, maxsplit=1)[0])
    if tab.source:
        parts.append(tab.source)
    if tab.muted:
        parts.append("Muted")
    elif tab.audible:
        parts.append("Playing")
    return "  ·  ".join(parts)


def _app_icon(tab: TabView) -> dict:
    if tab.app:
        return {"type": "fileicon", "src": tab.app}
    return {"type": "symbol", "src": "globe"}


def ui_entry(tab: TabView, icons: dict) -> dict:
    """One dataview row. `component.property` keys feed the cell in
    `components/tablist.json`; the plain `tab*` keys are read by actions."""
    icon = icons.get(tab.host)
    entry = {
        "uid": tab.uid,
        "title.text": defuse(tab.title),
        "subtitle.text": defuse(subtitle(tab)),
        "favicon.image": {"type": "file", "src": str(icon)} if icon else _app_icon(tab),
        "browsericon.image": _app_icon(tab),
        "browsericon.tooltip": defuse(tab.source),
    }
    if tab.pinned:
        entry["pin.hidden"] = False
    if tab.muted or tab.audible:
        entry["sound.hidden"] = False
        entry["sound.image"] = {
            "type": "symbol",
            "src": "speaker.slash.fill" if tab.muted else "speaker.wave.2.fill",
        }
    clean_title = tab.title.replace("[", "").replace("]", "")
    entry.update(
        {
            "match": defuse(match_text(tab)),
            "taburl": defuse(tab.url),
            "tabtitle": defuse(tab.title),
            "tabmd": defuse(f"[{clean_title}]({tab.url})"),
            "tabgroup": defuse(tab.group),
        }
    )
    return entry


def data_filter() -> dict:
    """Repeated on every data update so replacing the data keeps it. Alfred
    does not preprocess `data.filter`, so the placeholder stays live."""
    return {
        "type": "basic",
        "args": "match",
        "match": "{value:search.text}",
        "matchmode": "wordmatchanyorder",
    }


def fingerprint(data: list) -> str:
    return hashlib.blake2b(json.dumps(data, sort_keys=True).encode(), digest_size=8).hexdigest()


def status_text(tab_count: int, browsers: list) -> str:
    if not browsers:
        return "No browser connected"
    tabs = "1 tab" if tab_count == 1 else f"{tab_count} tabs"
    return f"{tabs}  ·  {', '.join(browsers)}"


CURRENT = object()  # keep the user's selection
FIRST = ""  # reset to the first visible row


def ui_update(data: list, status: str, select, placeholder: str, checklist=None) -> dict:
    """JSON Actions replacing the list, or showing the setup checklist when
    `checklist` is given."""
    setup_mode = checklist is not None
    variables = {"setupmode": "1" if setup_mode else ""}
    if select is CURRENT:
        variables["cursel"] = "{value:tabs.selected.uid}"
        select = "{var:cursel}"
    ids = {
        "tabs": {
            "hidden": setup_mode,
            "placeholder": placeholder,
            "data": {"type": "basic", "src": data, "filter": data_filter(), "select": select},
        },
        "setuppanel": {"hidden": not setup_mode},
        "status": {"text": status},
    }
    if setup_mode:
        ids["setup"] = {"data": {"type": "basic", "src": checklist}}
    return {"variables": variables, "update": {"ids": ids}, "functions": {"ids": {"tabs": "filterdata"}}}


def script_filter(tabs: list, icons: dict, checklist=None) -> dict:
    """Result list; the setup checklist as results while no browser is connected."""
    items = []
    for tab in tabs:
        icon_path = icons.get(tab.host)
        if icon_path:
            icon = {"path": str(icon_path), "resize": "fit"}
        elif tab.app:
            icon = {"type": "fileicon", "path": tab.app, "resize": "fit"}
        else:
            icon = {"path": "icon.png", "resize": "fit"}
        items.append(
            {
                "uid": tab.uid,
                "title": f"📌 {tab.title}" if tab.pinned else tab.title,
                "subtitle": subtitle(tab),
                "arg": tab.uid,
                "match": match_text(tab),
                "autocomplete": tab.title,
                "icon": icon,
                "quicklookurl": tab.url,
                "action": {"url": tab.url},
                "text": {"copy": tab.url, "largetype": tab.url},
                "variables": {"tabaction": "focus"},
                "mods": {
                    "cmd": {
                        "arg": tab.url,
                        "subtitle": f"Copy URL: {tab.url}",
                        "variables": {"tabaction": "copy"},
                    },
                    "ctrl": {
                        "arg": tab.uid,
                        "subtitle": f"Close tab in {tab.source}",
                        "variables": {"tabaction": "close"},
                    },
                },
            }
        )
    for entry in checklist or []:
        action = entry["action"] or entry["takeover"]
        detail = entry["sdetail.text"]
        if entry["takeover"]:
            detail += ". ↩ makes it send them to Browvitab"
        items.append(
            {
                "title": entry["stitle.text"],
                "subtitle": detail,
                "arg": action,
                "valid": bool(action),
                "variables": {"tabaction": "setup"},
                "icon": {"path": "icon.png"},
            }
        )
    return {"skipknowledge": True, "strictmods": True, "iconresize": "fit", "items": items}
