# Proposing Browvitab for the Alfred Gallery

The Gallery does not take direct submissions. The Alfred team asks authors to
share a workflow on the
[Share your Workflows forum](https://www.alfredforum.com/forum/3-share-your-workflows/)
first, and invites stable, well-received workflows to submit officially.
Requirements are on the [submit page](https://alfred.app/submit/) and in the
[style guide](https://alfred.app/submit/styleguide/).

## Checklist

| Gallery requirement | Status |
| --- | --- |
| Shared on the forum | To do: post the draft below |
| README follows the style guide (`## Setup`, `## Usage`, kbd modifiers, screenshot) | Done: `workflow/README.alfred.md` |
| Keywords of three characters or more, user-configurable | Done: `tabs`, `tabl`, `tabs:setup` in Workflow Configuration |
| Icon at least 256×256 px | Done: 512×512, drawn by `scripts/icon.swift` |
| No auto-updater | Done |
| Never downloads or installs extra software | Done: the helper ships inside the workflow; `tabs:setup` copies that same file to `~/Library/Application Support/browvitab` so browser manifests survive synced preferences moving |
| No Gatekeeper workarounds such as removing quarantine | Done: `install.sh` no longer strips quarantine |
| Compiled binaries signed and notarised | **Missing.** Needs a *Developer ID Application* certificate. This Mac only has an *Apple Development* identity, which cannot be notarised for distribution |

### Signing releases

`.github/workflows/release.yml` signs and notarises the universal binary when
these repository secrets exist:

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE` | The Developer ID Application certificate and key as a base64 `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | Its export password |
| `MACOS_SIGNING_IDENTITY` | e.g. `Developer ID Application: Chmouel Boudjnah (TEAMID)` |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | For `notarytool` (an app-specific password) |

Push a new `v*` tag once they are set; the release then carries a notarised
helper.

## Forum post draft

Title: **Browvitab: switch, copy and close Firefox, Chrome and Brave tabs from one Alfred 6 window**

> I wanted one tab switcher for Firefox and the Chromium browsers I keep open
> side by side, so I wrote one for Alfred 6.
>
> Browvitab uses the new Alfred UI: a native window where typing filters
> instantly, <kbd>↩</kbd> jumps to the tab in the right window (separate
> Firefox profiles stay separate), <kbd>⌘</kbd><kbd>↩</kbd> copies the URL,
> <kbd>⌃</kbd><kbd>↩</kbd> closes the tab without leaving the list, and
> <kbd>⌘</kbd><kbd>K</kbd> opens more actions. The list refreshes while it is
> open when tabs change in the browser. If you prefer the classic result list,
> `tabl` shows the same tabs there.
>
> Firefox has no AppleScript tab support, so Browvitab gets tabs from the open
> source Vicinae browser extension (Firefox Add-ons and Chrome Web Store).
> `tabs:setup` connects it. Site icons come from the browsers' own caches;
> nothing goes over the network.
>
> Download and source: https://github.com/chmouel/alfred-browvitab
>
> Feedback welcome, especially from people running several browsers or
> Firefox profiles.

Attach `docs/images/demo.gif` to the post.
