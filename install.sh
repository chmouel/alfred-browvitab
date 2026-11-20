#!/bin/bash
# Install Browvitab for Alfred 6.
#
#   ./install.sh              connect the browsers and import the workflow
#   ./install.sh --force      also replace manifests owned by Vicinae or others
#   ./install.sh --no-import  only connect the browsers
set -euo pipefail

cd "$(dirname "$0")"
FORCE=""
IMPORT=1
for arg in "$@"; do
  case "$arg" in
  --force) FORCE="--force" ;;
  --no-import) IMPORT=0 ;;
  -h | --help)
    sed -n '2,7p' "$0"
    exit 0
    ;;
  *)
    echo "unknown option: $arg" >&2
    exit 2
    ;;
  esac
done

BIN=workflow/tabs.alfredui/bin/browvitab

echo "==> Packaging the workflow"
python3 scripts/build.py >/dev/null

echo "==> Connecting the browsers"
out=$("$BIN" install $FORCE)
echo "$out"
if [[ -z $FORCE ]] && grep -q "left alone" <<<"$out"; then
  echo
  echo "Some browsers already send their tabs to another program (usually Vicinae)."
  read -r -p "Replace those manifests so Alfred gets the tabs? [y/N] " answer
  if [[ $answer == [yY]* ]]; then
    "$BIN" install --force
  fi
fi

if [[ $IMPORT == 1 ]]; then
  echo "==> Importing the workflow into Alfred"
  open dist/Browvitab.alfredworkflow
fi

cat <<'EOF2'

Next:
  1. Install the Vicinae extension if needed:
     Firefox: https://addons.mozilla.org/firefox/addon/vicinae/
     Chrome/Brave: https://chromewebstore.google.com/detail/vicinae-integration/kcmipingpfbohfjckomimmahknoddnke
  2. The extension reconnects within a minute (or toggle it off and on).
  3. Type "tabs" in Alfred, or press ⌥⌘T.
EOF2
