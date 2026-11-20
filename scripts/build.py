#!/usr/bin/env python3
"""Generate workflow/info.plist and package dist/Browvitab.alfredworkflow.

The workflow is described here rather than hand-edited in Alfred so the
object graph is reviewable and reproducible.
"""

import os
import plistlib
import shutil
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WF = os.path.join(ROOT, "workflow")
DIST = os.path.join(ROOT, "dist")
BIN = "./tabs.alfredui/bin/raffi-tabs"
VERSION = os.environ.get("BROWVITAB_VERSION", "0.1.0")
BUNDLE_ID = "com.chmouel.alfred-browvitab"

CMD = 1048576
ALT = 524288
CTRL = 262144

U = {
    "ui_keyword": "5C1A6A0E-0001-4F00-9000-000000000001",
    "hotkey": "5C1A6A0E-0002-4F00-9000-000000000002",
    "ui": "5C1A6A0E-0003-4F00-9000-000000000003",
    "focus": "5C1A6A0E-0004-4F00-9000-000000000004",
    "filter": "5C1A6A0E-0005-4F00-9000-000000000005",
    "copy": "5C1A6A0E-0006-4F00-9000-000000000006",
    "close": "5C1A6A0E-0007-4F00-9000-000000000007",
    "setup_kw": "5C1A6A0E-0008-4F00-9000-000000000008",
    "setup": "5C1A6A0E-0009-4F00-9000-000000000009",
    "notify": "5C1A6A0E-000A-4F00-9000-00000000000A",
    "external": "5C1A6A0E-000B-4F00-9000-00000000000B",
}


def script(code):
    return {
        "concurrently": False,
        "escaping": 102,
        "script": code,
        "scriptargtype": 1,
        "scriptfile": "",
        "type": 0,
    }


OBJECTS = [
    {
        "uid": U["ui_keyword"],
        "type": "alfred.workflow.input.keyword",
        "version": 1,
        "config": {
            "keyword": "{var:ui_keyword}",
            "text": "Browser Tabs",
            "subtext": "Search and switch browser tabs",
            "argumenttype": 1,
            "withspace": True,
        },
    },
    {
        "uid": U["hotkey"],
        "type": "alfred.workflow.trigger.hotkey",
        "version": 2,
        "config": {
            "hotkey": 17,
            "hotmod": CMD | ALT,
            "hotstring": "T",
            "action": 0,
            "argument": 0,
            "relatedAppsMode": 0,
            "focusedappvariable": False,
            "focusedappvariablename": "",
            "leftcursor": False,
            "modsmode": 0,
        },
    },
    {
        "uid": U["external"],
        "type": "alfred.workflow.trigger.external",
        "version": 1,
        "config": {"triggerid": "tabs", "availableviaurlhandler": True},
    },
    {
        "uid": U["ui"],
        "type": "alfred.workflow.userinterface.alfredui",
        "version": 1,
        "config": {"path": "tabs.alfredui", "stackview": True, "launchmode": 0},
    },
    {
        "uid": U["focus"],
        "type": "alfred.workflow.action.script",
        "version": 2,
        "config": script(
            '# Only the "Switch" path focuses; copy/close finish with other values.\n'
            '[ "${tabaction:-focus}" = focus ] && [ -n "$1" ] || exit 0\n'
            f'exec {BIN} focus "$1"\n'
        ),
    },
    {
        "uid": U["filter"],
        "type": "alfred.workflow.input.scriptfilter",
        "version": 3,
        "config": {
            "keyword": "{var:list_keyword}",
            "title": "Browser Tabs (list)",
            "subtext": "Classic result list of browser tabs",
            "runningsubtext": "Loading tabs…",
            "argumenttype": 1,
            "argumenttreatemptyqueryasnil": True,
            "argumenttrimmode": 0,
            "withspace": True,
            "alfredfiltersresults": True,
            "alfredfiltersresultsmatchmode": 2,
            "queuemode": 1,
            "queuedelaymode": 0,
            "queuedelaycustom": 3,
            "queuedelayimmediatelyinitially": True,
            "escaping": 102,
            "scriptargtype": 1,
            "scriptfile": "",
            "type": 0,
            "script": f"exec {BIN} filter\n",
        },
    },
    {
        "uid": U["copy"],
        "type": "alfred.workflow.output.clipboard",
        "version": 3,
        "config": {
            "autopaste": False,
            "clipboardtext": "{query}",
            "ignoredynamicplaceholders": True,
            "transient": False,
        },
    },
    {
        "uid": U["close"],
        "type": "alfred.workflow.action.script",
        "version": 2,
        "config": script(f'exec {BIN} close "$1"\n'),
    },
    {
        "uid": U["setup_kw"],
        "type": "alfred.workflow.input.keyword",
        "version": 1,
        "config": {
            "keyword": "{var:ui_keyword}:setup",
            "text": "Set up browser tabs",
            "subtext": "Register the native messaging host with installed browsers",
            "argumenttype": 2,
            "withspace": False,
        },
    },
    {
        "uid": U["setup"],
        "type": "alfred.workflow.action.script",
        "version": 2,
        "config": script(
            f'out=$({BIN} install 2>&1)\n'
            'case "$out" in *"left alone"*) echo "Some manifests belong to another host (Vicinae?). Run install.sh --force to replace them.";;\n'
            '  *) echo "Host registered. The extension reconnects within a minute.";; esac\n'
        ),
    },
    {
        "uid": U["notify"],
        "type": "alfred.workflow.output.notification",
        "version": 1,
        "config": {
            "lastpathcomponent": False,
            "onlyshowifquerypopulated": True,
            "removeextension": False,
            "text": "{query}",
            "title": "Browser Tabs",
        },
    },
]


def conn(dest, mods=0):
    return {"destinationuid": dest, "modifiers": mods, "modifiersubtext": "", "vitoclose": False}


CONNECTIONS = {
    U["ui_keyword"]: [conn(U["ui"])],
    U["hotkey"]: [conn(U["ui"])],
    U["external"]: [conn(U["ui"])],
    U["ui"]: [conn(U["focus"])],
    U["filter"]: [conn(U["focus"]), conn(U["copy"], CMD), conn(U["close"], CTRL)],
    U["setup_kw"]: [conn(U["setup"])],
    U["setup"]: [conn(U["notify"])],
}

UIDATA = {
    U["ui_keyword"]: {"xpos": 40, "ypos": 40},
    U["hotkey"]: {"xpos": 40, "ypos": 150},
    U["external"]: {"xpos": 40, "ypos": 240},
    U["ui"]: {"xpos": 260, "ypos": 100},
    U["focus"]: {"xpos": 480, "ypos": 100},
    U["filter"]: {"xpos": 40, "ypos": 360},
    U["copy"]: {"xpos": 480, "ypos": 300},
    U["close"]: {"xpos": 480, "ypos": 420},
    U["setup_kw"]: {"xpos": 40, "ypos": 520},
    U["setup"]: {"xpos": 260, "ypos": 520},
    U["notify"]: {"xpos": 480, "ypos": 540},
}

USER_CONFIG = [
    {
        "type": "textfield",
        "variable": "ui_keyword",
        "label": "Tabs keyword",
        "description": "Opens the native Alfred UI tab switcher",
        "config": {"default": "tabs", "placeholder": "tabs", "required": True, "trim": True},
    },
    {
        "type": "textfield",
        "variable": "list_keyword",
        "label": "List keyword",
        "description": "Opens the classic Script Filter result list",
        "config": {"default": "tabl", "placeholder": "tabl", "required": True, "trim": True},
    },
    {
        "type": "popupbutton",
        "variable": "button_effect",
        "label": "Button style",
        "description": "Glass needs macOS 26 or later",
        "config": {"default": "default", "pairs": [["Standard", "default"], ["Glass", "glass"]]},
    },
]

README = open(os.path.join(ROOT, "workflow", "README.alfred.md")).read()


def info_plist():
    return {
        "bundleid": BUNDLE_ID,
        "name": "Browvitab",
        "description": "Switch, copy and close Firefox, Chrome and Brave tabs",
        "category": "Productivity",
        "createdby": "Chmouel Boudjnah",
        "webaddress": "https://github.com/chmouel/alfred-browvitab",
        "version": VERSION,
        "readme": README,
        "disabled": False,
        "objects": OBJECTS,
        "connections": CONNECTIONS,
        "uidata": UIDATA,
        "userconfigurationconfig": USER_CONFIG,
        "variablesdontexport": [],
    }


def build_binary():
    # CI passes a universal (and possibly signed) binary through BROWVITAB_BINARY.
    binary = os.environ.get("BROWVITAB_BINARY")
    if not binary:
        subprocess.run(["cargo", "build", "--release"], cwd=ROOT, check=True)
        binary = os.path.join(ROOT, "target", "release", "raffi-tabs")
    dest = os.path.join(WF, "tabs.alfredui", "bin", "raffi-tabs")
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    shutil.copy2(binary, dest)
    os.chmod(dest, 0o755)


def package():
    os.makedirs(DIST, exist_ok=True)
    out = os.path.join(DIST, "Browvitab.alfredworkflow")
    if os.path.exists(out):
        os.remove(out)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for base, _, files in os.walk(WF):
            for name in sorted(files):
                if name in (".DS_Store", "README.alfred.md"):
                    continue
                path = os.path.join(base, name)
                rel = os.path.relpath(path, WF)
                info = zipfile.ZipInfo.from_file(path, rel)
                with open(path, "rb") as f:
                    z.writestr(info, f.read(), zipfile.ZIP_DEFLATED)
    return out


def main():
    if "--no-cargo" not in sys.argv:
        build_binary()
    with open(os.path.join(WF, "info.plist"), "wb") as f:
        plistlib.dump(info_plist(), f)
    print(package())


if __name__ == "__main__":
    main()
