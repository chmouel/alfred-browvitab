"""macOS process lookups and application activation through ctypes."""

from __future__ import annotations

import ctypes
import os
import subprocess
from pathlib import Path

_LIBPROC = None


def _libproc():
    global _LIBPROC
    if _LIBPROC is None:
        _LIBPROC = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    return _LIBPROC


def exe_path(pid: int) -> str | None:
    """Executable path of `pid`."""
    buf = ctypes.create_string_buffer(4096)
    n = _libproc().proc_pidpath(ctypes.c_int(pid), buf, ctypes.c_uint32(len(buf)))
    if n <= 0:
        return None
    return buf.raw[:n].decode("utf-8", "replace")


def parent_of(pid: int) -> int | None:
    try:
        out = subprocess.run(
            ["/bin/ps", "-o", "ppid=", "-p", str(pid)],
            capture_output=True,
            text=True,
            timeout=2,
        ).stdout.strip()
        return int(out) if out else None
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def alive(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def app_bundle(exe: str) -> str | None:
    """The outermost `.app` containing `exe`, so helpers resolve to the browser."""
    found = None
    for parent in reversed(Path(exe).parents):
        if parent.suffix == ".app":
            found = str(parent)
            break
    return found


def gecko_profile_dir(pid: int) -> str | None:
    """The Firefox-family profile `pid` runs, from the `.parentlock` it holds open."""
    try:
        out = subprocess.run(
            ["/usr/sbin/lsof", "-a", "-p", str(pid), "-Fn"],
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for line in out.splitlines():
        if line.startswith("n") and line.endswith("/.parentlock"):
            return line[1 : -len("/.parentlock")]
    return None


def gecko_profile_name(profile_dir: str) -> str:
    """The name `profiles.ini` gives a profile directory, or a fallback from its name."""
    directory = Path(profile_dir)
    for root in list(directory.parents)[:3]:
        try:
            ini = (root / "profiles.ini").read_text(errors="replace")
        except OSError:
            continue
        section: dict = {}
        sections = []
        for raw in ini.splitlines():
            line = raw.strip()
            if line.startswith("["):
                sections.append(section)
                section = {}
            elif "=" in line:
                key, value = line.split("=", 1)
                section[key] = value
        sections.append(section)
        for s in sections:
            if "Name" not in s or "Path" not in s:
                continue
            full = root / s["Path"] if s.get("IsRelative", "1") != "0" else Path(s["Path"])
            if full == directory:
                return s["Name"]
    name = directory.name
    return name.split(".", 1)[1] if "." in name else name


# Objective-C runtime, for NSRunningApplication / NSWorkspace.
_OBJC = None


def _objc():
    global _OBJC
    if _OBJC is None:
        ctypes.cdll.LoadLibrary("/System/Library/Frameworks/AppKit.framework/AppKit")
        lib = ctypes.cdll.LoadLibrary("/usr/lib/libobjc.A.dylib")
        lib.objc_getClass.restype = ctypes.c_void_p
        lib.objc_getClass.argtypes = [ctypes.c_char_p]
        lib.sel_registerName.restype = ctypes.c_void_p
        lib.sel_registerName.argtypes = [ctypes.c_char_p]
        _OBJC = lib
    return _OBJC


def _send(restype, *argtypes):
    """objc_msgSend with an exact prototype (required on arm64)."""
    lib = _objc()
    proto = ctypes.CFUNCTYPE(restype, ctypes.c_void_p, ctypes.c_void_p, *argtypes)
    return proto(ctypes.cast(lib.objc_msgSend, ctypes.c_void_p).value)


def _sel(name: str):
    return _objc().sel_registerName(name.encode())


def activate_pid(pid: int) -> bool:
    """Bring the app owning `pid` to the front. Per process, so two Firefox
    instances (profiles) stay apart."""
    try:
        cls = _objc().objc_getClass(b"NSRunningApplication")
        if not cls:
            return False
        app = _send(ctypes.c_void_p, ctypes.c_int)(cls, _sel("runningApplicationWithProcessIdentifier:"), pid)
        if not app:
            return False
        all_windows, ignoring_other_apps = 1, 2
        return bool(
            _send(ctypes.c_bool, ctypes.c_ulong)(app, _sel("activateWithOptions:"), all_windows | ignoring_other_apps)
        )
    except (OSError, AttributeError):
        return False


def frontmost_pid() -> int | None:
    try:
        cls = _objc().objc_getClass(b"NSWorkspace")
        get = _send(ctypes.c_void_p)
        workspace = get(cls, _sel("sharedWorkspace"))
        app = get(workspace, _sel("frontmostApplication")) if workspace else None
        if not app:
            return None
        return int(_send(ctypes.c_int)(app, _sel("processIdentifier")))
    except (OSError, AttributeError):
        return None
