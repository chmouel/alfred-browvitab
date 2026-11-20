"""The browsers Browvitab knows on macOS."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

MOZILLA_HOSTS = "Mozilla/NativeMessagingHosts"


@dataclass(frozen=True)
class Browser:
    id: str  # matches the id the host reports
    name: str
    kind: str  # "gecko" or "chromium"
    root: str  # profile root under ~/Library/Application Support
    app: str  # bundle name in /Applications
    bundle_id: str
    extra_manifest_dirs: tuple = ()

    @property
    def manifest_dirs(self) -> tuple:
        if self.kind == "gecko":
            # Firefox and its forks read the shared Mozilla directory.
            return (MOZILLA_HOSTS,) + self.extra_manifest_dirs
        return (f"{self.root}/NativeMessagingHosts",)


BROWSERS = (
    Browser("firefox", "Firefox", "gecko", "Firefox", "Firefox", "org.mozilla.firefox"),
    Browser("zen", "Zen", "gecko", "zen", "Zen", "app.zen-browser.zen"),
    Browser("floorp", "Floorp", "gecko", "Floorp", "Floorp", "one.ablaze.floorp"),
    Browser("waterfox", "Waterfox", "gecko", "Waterfox", "Waterfox", "net.waterfox.waterfox"),
    Browser(
        "librewolf",
        "LibreWolf",
        "gecko",
        "librewolf",
        "LibreWolf",
        "io.gitlab.librewolf-community",
        ("librewolf/NativeMessagingHosts",),
    ),
    Browser("google-chrome", "Google Chrome", "chromium", "Google/Chrome", "Google Chrome", "com.google.Chrome"),
    Browser(
        "google-chrome-beta",
        "Google Chrome Beta",
        "chromium",
        "Google/Chrome Beta",
        "Google Chrome Beta",
        "com.google.Chrome.beta",
    ),
    Browser(
        "google-chrome-dev",
        "Google Chrome Dev",
        "chromium",
        "Google/Chrome Dev",
        "Google Chrome Dev",
        "com.google.Chrome.dev",
    ),
    Browser(
        "google-chrome-canary",
        "Google Chrome Canary",
        "chromium",
        "Google/Chrome Canary",
        "Google Chrome Canary",
        "com.google.Chrome.canary",
    ),
    Browser("chromium", "Chromium", "chromium", "Chromium", "Chromium", "org.chromium.Chromium"),
    Browser("brave-browser", "Brave", "chromium", "BraveSoftware/Brave-Browser", "Brave Browser", "com.brave.Browser"),
    Browser(
        "microsoft-edge", "Microsoft Edge", "chromium", "Microsoft Edge", "Microsoft Edge", "com.microsoft.edgemac"
    ),
    Browser("vivaldi", "Vivaldi", "chromium", "Vivaldi", "Vivaldi", "com.vivaldi.Vivaldi"),
    Browser("arc", "Arc", "chromium", "Arc/User Data", "Arc", "company.thebrowser.Browser"),
    Browser("thorium", "Thorium", "chromium", "Thorium", "Thorium", "org.chromium.Thorium"),
    Browser("helium", "Helium", "chromium", "net.imput.helium", "Helium", "net.imput.helium"),
)

BY_ID = {b.id: b for b in BROWSERS}
# Developer and Nightly builds share Firefox's profile directory.
GECKO_IDS = {b.id for b in BROWSERS if b.kind == "gecko"} | {"firefox-developer", "firefox-nightly"}


def lookup(browser_id: str) -> Browser | None:
    if browser_id in ("firefox-developer", "firefox-nightly"):
        return BY_ID["firefox"]
    return BY_ID.get(browser_id)


def app_path(browser: Browser) -> Path | None:
    for base in (Path("/Applications"), Path.home() / "Applications"):
        path = base / f"{browser.app}.app"
        if path.is_dir():
            return path
    return None
