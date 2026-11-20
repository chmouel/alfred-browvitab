#!/bin/bash
# Remove Browvitab: its native messaging manifests (never Vicinae's), the
# installed copy and caches, and optionally the Alfred workflow.
#
#   ./uninstall.sh              remove manifests, the installed copy and caches
#   ./uninstall.sh --workflow   also delete the workflow from Alfred
set -euo pipefail
cd "$(dirname "$0")"

workflow/tabs.alfredui/bin/browvitab uninstall

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
rm -f "/tmp/browvitab-$(id -u)/alfred-ui-watch.pid" "/tmp/browvitab-$(id -u)/trace"
echo "Running browser hosts stop when their browser quits or the extension is disabled."
