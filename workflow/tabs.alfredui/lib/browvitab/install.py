"""Native messaging manifest registration on macOS.

Manifests Browvitab did not write (Vicinae's, symbolic links, anything else)
are never replaced without `force`, and uninstall only removes its own.
"""

from __future__ import annotations

import json
import os
import shutil
import stat
from dataclasses import dataclass
from pathlib import Path

from .browsers import BROWSERS
from .host import CHROME_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME

# Written into every manifest we install, to tell them apart from others.
MANIFEST_DESCRIPTION = "Browvitab native messaging host for the Vicinae browser extension"


@dataclass
class Target:
    browsers: list  # display names of the browsers reading this directory
    ids: set
    dir: Path
    kind: str


def manifest_targets(support: Path, only: set | None = None) -> list:
    """Manifest directories of the browsers that have a profile directory,
    one entry per directory."""
    targets: dict = {}
    for browser in BROWSERS:
        if not (support / browser.root).is_dir() or (only and browser.id not in only):
            continue
        for rel in browser.manifest_dirs:
            target = targets.setdefault(rel, Target([], set(), support / rel, browser.kind))
            if browser.name not in target.browsers:
                target.browsers.append(browser.name)
            target.ids.add(browser.id)
    return list(targets.values())


def manifest_json(kind: str, exe: Path) -> str:
    manifest = {
        "name": HOST_NAME,
        "description": MANIFEST_DESCRIPTION,
        "path": str(exe),
        "type": "stdio",
    }
    if kind == "chromium":
        manifest["allowed_origins"] = [f"chrome-extension://{CHROME_EXTENSION_ID}/"]
    else:
        manifest["allowed_extensions"] = [FIREFOX_EXTENSION_ID]
    return json.dumps(manifest, indent=2) + "\n"


def manifest_owner(path: Path):
    """None when absent; ("ours", program); ("vicinae",); or ("other", why)."""
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(st.st_mode):
        return ("other", "it is a symbolic link")
    if not stat.S_ISREG(st.st_mode):
        return ("other", "it is not a regular file")
    try:
        manifest = json.loads(Path(path).read_text())
        if not isinstance(manifest, dict):
            raise ValueError
    except (OSError, ValueError):
        return ("other", "it cannot be read as a manifest")
    program = manifest.get("path")
    if not isinstance(program, str):
        return ("other", "it names no program")
    if manifest.get("description") == MANIFEST_DESCRIPTION:
        return ("ours", program)
    if "vicinae" in Path(program).name.lower():
        return ("vicinae",)
    return ("other", f"it starts {program}")


def _write_manifest(directory: Path, path: Path, content: str, replace: str) -> None:
    """Write through a temporary file. `replace` is "any", "absent" (a hard
    link, which fails if a manifest appeared meanwhile) or "ours" (only
    replace a manifest that is still ours)."""
    directory.mkdir(parents=True, exist_ok=True)
    temp = directory / f".{HOST_NAME}.json.{os.getpid()}.tmp"
    try:
        with open(temp, "x") as f:
            f.write(content)
            f.flush()
            os.fsync(f.fileno())
        os.chmod(temp, 0o644)
        owner = manifest_owner(path) if replace == "ours" else None
        if replace == "any" or (owner and owner[0] == "ours"):
            os.replace(temp, path)
        elif replace == "absent" or owner is None:
            os.link(temp, path)
        else:
            raise FileExistsError("another program replaced the manifest meanwhile")
    finally:
        try:
            temp.unlink()
        except FileNotFoundError:
            pass


@dataclass
class Report:
    browsers: list
    path: Path
    outcome: str  # written, unchanged, kept, removed, failed
    detail: str = ""


def install_manifests(support: Path, exe: Path, force: bool = False, only: set | None = None) -> list:
    """Point every browser's manifest at `exe`. `only` limits it to some
    browser ids (and implies `force`, to take one browser over from Vicinae)."""
    force = force or bool(only)
    reports = []
    for target in manifest_targets(support, only):
        browsers = target.browsers
        path = target.dir / f"{HOST_NAME}.json"
        content = manifest_json(target.kind, exe)
        owner = manifest_owner(path)
        is_link = os.path.islink(path)
        if force:
            keep, replace = None, "any"
        elif owner is None:
            keep, replace = None, "absent"
        elif owner[0] == "ours":
            keep, replace = None, "ours"
        elif owner[0] == "vicinae":
            keep, replace = "it starts Vicinae", ""
        else:
            keep, replace = owner[1], ""
        try:
            existing = None if is_link else Path(path).read_text()
        except OSError:
            existing = None
        if existing == content:
            reports.append(Report(browsers, path, "unchanged"))
        elif keep:
            reports.append(Report(browsers, path, "kept", keep))
        else:
            try:
                _write_manifest(target.dir, path, content, replace)
                reports.append(Report(browsers, path, "written"))
            except OSError as error:
                reports.append(Report(browsers, path, "failed", str(error)))
    return reports


def uninstall_manifests(support: Path) -> list:
    reports = []
    for target in manifest_targets(support):
        browsers = target.browsers
        path = target.dir / f"{HOST_NAME}.json"
        owner = manifest_owner(path)
        if not owner or owner[0] != "ours":
            continue
        try:
            path.unlink()
            reports.append(Report(browsers, path, "removed"))
        except OSError as error:
            reports.append(Report(browsers, path, "failed", str(error)))
    return reports


def status(support: Path) -> list:
    return [
        (t.browsers, t.dir / f"{HOST_NAME}.json", manifest_owner(t.dir / f"{HOST_NAME}.json"))
        for t in manifest_targets(support)
    ]


def support_dir() -> Path:
    home = os.environ.get("HOME")
    if not home:
        raise OSError("HOME is not set")
    return Path(home) / "Library" / "Application Support"


def install_dir(support: Path) -> Path:
    return support / "browvitab"


def install_files(support: Path, source_root: Path) -> Path:
    """Copy `bin/` and `lib/` of the running copy to a stable place outside the
    (possibly synced) Alfred preferences. Returns the host launcher path."""
    dest = install_dir(support)
    launcher = dest / "bin" / "browvitab"
    if source_root.resolve() == dest.resolve():
        return launcher
    dest.mkdir(parents=True, exist_ok=True)
    staging = dest / f".staging-{os.getpid()}"
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir()
    ignore = shutil.ignore_patterns("__pycache__", "*.pyc")
    for part in ("bin", "lib"):
        shutil.copytree(source_root / part, staging / part, ignore=ignore)
    for part in ("bin", "lib"):
        old = dest / f".old-{part}-{os.getpid()}"
        if (dest / part).exists():
            os.replace(dest / part, old)
        os.replace(staging / part, dest / part)
        shutil.rmtree(old, ignore_errors=True)
    shutil.rmtree(staging, ignore_errors=True)
    os.chmod(launcher, 0o755)
    return launcher


def installed_version(support: Path) -> str | None:
    init = install_dir(support) / "lib" / "browvitab" / "__init__.py"
    try:
        for line in init.read_text().splitlines():
            if line.startswith("__version__"):
                return line.split("=", 1)[1].strip().strip("\"'")
    except OSError:
        pass
    return None
