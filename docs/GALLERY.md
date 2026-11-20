# Getting Browvitab into the Alfred Gallery

The Gallery takes no direct submissions. The Alfred team asks authors to share
a workflow on the
[Share your Workflows forum](https://www.alfredforum.com/forum/3-share-your-workflows/),
and invites the ones people trust to submit officially. The rules are on the
[submit page](https://alfred.app/submit/) and in the
[style guide](https://alfred.app/submit/styleguide/).

## Where Browvitab stands

| Gallery rule | Status |
| --- | --- |
| Shared on the forum first | To do: post the text below |
| README in the Gallery style (`## Setup`, `## Usage`, screenshot, `<kbd>` keys) | Done in `workflow/README.alfred.md` |
| Keywords of three characters or more, user-configurable | Done: `tabs`, `tabl`, `tabs:setup` |
| Icon of at least 256×256 px | Done: 512×512 |
| No auto-updater | Done |
| Never downloads or installs software | Done: everything ships in the workflow. Setup copies those same files to `~/Library/Application Support/browvitab` |
| No Gatekeeper workarounds | Done |
| Compiled binaries signed and notarised | Not needed: only Python and shell scripts |
| Dependencies declared | Python 3.9+, to list in the submission (Homebrew `python` or Xcode Command Line Tools) |

## Steps

1. Tag a release (`git tag v0.2.0 && git push origin v0.2.0`). CI attaches
   `Browvitab.alfredworkflow` to it.
2. Create an account on the [Alfred forum](https://www.alfredforum.com) and
   post the text below in *Share your Workflows*. Attach `docs/images/demo.gif`.
3. Answer questions and fix what people report for a few weeks. The Alfred team
   watches that forum and sends an invitation when a workflow looks stable.
4. When invited, submit through the process they send you, listing Python 3
   as the dependency.
5. Later updates go through
   [an issue on alfredapp/gallery-edits](https://github.com/alfredapp/gallery-edits/issues/new?template=02_update_workflow.yml).
   Users who installed from GitHub keep receiving the Gallery updates.

## Forum post

Title: **Browvitab: switch, copy and close Firefox, Chrome and Brave tabs from one Alfred 6 window**

> I keep Firefox and a couple of Chromium browsers open side by side and
> wanted one tab switcher for all of them, so I wrote one for Alfred 6.
>
> Browvitab uses the new Alfred UI. Typing filters instantly, <kbd>↩</kbd>
> jumps to the tab in the right window (separate Firefox profiles stay
> separate), <kbd>⌘</kbd><kbd>↩</kbd> copies the URL, <kbd>⌃</kbd><kbd>↩</kbd>
> closes the tab without leaving the list, and <kbd>⌘</kbd><kbd>K</kbd> opens
> more actions. The list updates while it is open. `tabl` shows the same tabs
> as regular results.
>
> Firefox has no AppleScript tab support, so Browvitab reads tabs through the
> open source Vicinae browser extension. The first time you type `tabs`, a
> checklist walks you through adding it and connecting your browsers. Site
> icons come from the browsers' own caches and nothing goes over the network.
>
> It needs Python 3.9 or later.
>
> Download and source: https://github.com/chmouel/alfred-browvitab
>
> I'd like to hear from people running several browsers or Firefox profiles.
