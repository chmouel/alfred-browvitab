#!/bin/bash
# Remove Browvitab: our native messaging manifests (never Vicinae's),
# the installed host binary and caches, and optionally the Alfred workflow.
#
#   ./uninstall.sh              remove host, manifests and caches
#   ./uninstall.sh --workflow   also delete the workflow from Alfred
set -euo pipefail
cd "$(dirname "$0")"

HOST="$HOME/Library/Application Support/browvitab/bin/browvitab"
BIN=workflow/tabs.alfredui/bin/browvitab
if [[ -x $HOST ]]; then
  # Copy first: uninstall deletes the directory holding $HOST.
  tmp=$(mktemp -t browvitab)
  cp "$HOST" "$tmp"
  "$tmp" uninstall
  rm -f "$tmp"
elif [[ -x $BIN ]]; then
  BROWVITAB_NO_COPY=1 "$BIN" uninstall
else
  echo "browvitab binary not found; nothing to unregister."
fi

if [[ ${1:-} == --workflow ]]; then
  prefs=$(python3 -c 'import json,os;print(os.path.expanduser(json.load(open(os.path.expanduser("~/Library/Application Support/Alfred/prefs.json")))["current"]))' 2>/dev/null ||
    echo "$HOME/Library/Application Support/Alfred/Alfred.alfredpreferences")
  for dir in "$prefs"/workflows/*/; do
    if [[ -f $dir/info.plist ]] && [[ $(/usr/libexec/PlistBuddy -c 'Print :bundleid' "$dir/info.plist" 2>/dev/null) == com.chmouel.alfred-browvitab ]]; then
      echo "Removing workflow ${dir%/}"
      rm -rf "${dir%/}"
    fi
  done
fi
rm -rf "/tmp/browvitab-$(id -u)/alfred-ui-watch.pid" "/tmp/browvitab-$(id -u)/trace"
echo "Running browser hosts stop when their browser quits or the extension is disabled."
