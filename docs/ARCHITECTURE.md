# How Browvitab works

The Vicinae browser extension starts a small helper program (the *host*)
through the browser's native messaging. Each running browser gets its own
host, which keeps that browser's tab list and answers requests on a private
Unix socket. The Alfred workflow asks every host for its tabs, shows them in a
native Alfred UI, and sends focus or close requests back.

| Path | What it holds |
| --- | --- |
| `src/host.rs` | The native messaging host and the socket client |
| `src/tabs.rs` | Tab ordering and search ranking |
| `src/alfred.rs` | Data for the Alfred UI and JSON for the Script Filter |
| `src/favicons.rs` | Site icons read from the browsers' own favicon databases |
| `src/install.rs` | Native messaging manifest registration |
| `src/macos.rs` | Process lookups and per-process app activation |
| `workflow/tabs.alfredui/` | The Alfred UI package |
| `scripts/build.py` | Builds the binary, writes `info.plist`, packages the workflow |
| `scripts/icon.swift` | Draws the workflow icon |
| `tests/fake_extension.py` | A stand-in for the browser extension, used in tests |

One binary, `browvitab`, plays both parts. When a browser starts it with the
native messaging arguments it runs as a host; otherwise it is the command-line
back end for the workflow (`browvitab --help`).

## The Alfred UI

The keyword, the hotkey and the external trigger all open `tabs.alfredui`
inside Alfred, with stacking on so <kbd>⎋</kbd> returns to Alfred. When you
pick a tab, the UI's `finish` action hands the tab id to a Run Script object
that switches to it.

The window is a border layout: search field on top, a `dataview` in the
middle and a footer of buttons. Each row is a border layout too, with the
favicon on the left, title and site in the middle, and the pin, sound and
browser icons on the right. The badge and the shared actions are reused
through a template and `{file:}` includes.

Typing never starts a process. The search field's `changed` event calls the
dataview's `filterdata`, and Alfred's `basic` filter matches a hidden `match`
field holding the title, the site and the address split into words, with the
`wordmatchanyorder` mode. Alfred keeps the data in its original order, so
once you pause (`typingstopped`), an async script sorts the list by match
quality and selects the best hit. If you have already moved the selection
with the arrow keys, it stays where you put it.

Every row has a uid of the form `<host pid>:<tab id>`. Data refreshes pass
`select` with the current selection, read through `{value:tabs.selected.uid}`
when the update is applied, so a refresh never moves you.

Copying, finishing, menus and selection changes are JSON Actions and run
inside Alfred. Only closing a tab, ranking and setup run the binary, and
those run asynchronously.

The UI uses only system colours and Alfred's theme colours
(`alfredSelectedResultBackgroundColor` and friends), so it follows light and
dark mode and your Alfred theme. The buttons can use the macOS 26 glass
effect from the Workflow Configuration.

### Keys that work, and one that does not

<kbd>⌘</kbd><kbd>↩</kbd>, <kbd>⌃</kbd><kbd>↩</kbd>, <kbd>⌘</kbd><kbd>K</kbd>
and the arrow keys are view shortcuts on the window's root view. Option-Return
never reaches those shortcuts: the focused search field swallows it and Alfred
logs no key event at all. Closing a tab therefore uses <kbd>⌃</kbd><kbd>↩</kbd>.

### Live refresh

The host bumps a `generation` counter whenever the extension reports a
change, and answers a `watch` request as soon as that counter moves. The UI's
pulse runs `browvitab ui-watch <fingerprint> <query>`:

1. List the hosts. If the tabs differ from what the UI shows, print one
   update and exit.
2. Otherwise send `watch` to every host and block. Wake up on the first
   change, when a browser connects or quits, when the UI closes, or after
   25 seconds, then exit. The pulse starts it again 100 ms later.

While the window is open, one process sits waiting on the browsers' own
events; a change shows up in about 200 ms. Closing the window removes the
watcher's pid file and the watcher exits within half a second. Nothing runs
in the background apart from the hosts the browsers start themselves.

Closing a tab from the list answers with fresh data and the next visible row
selected (or the previous one at the bottom), so you can close several tabs
in a row.

## The host on macOS

The host speaks the Vicinae extension's protocol: `browser/init`,
`browser/tabs-changed`, `browser/focus-tab` and `browser/close-tab`. Clients
send one JSON line per connection. Requests carry a monotonic deadline so a
late request is dropped instead of acted on, and the host checks that the
browser is reading before writing to it.

Requests to the extension carry an `id`. Extension 1.2 silently drops focus
and close requests without one.

Sockets live in `/tmp/browvitab-<uid>/browsers/<pid>.sock`, in a directory
only you can read. A socket is removed as stale only when its process is
gone, because macOS also refuses connections from a live host whose backlog
is full.

The host recognises its browser from the parent process's `.app` bundle.
Firefox and its forks run each profile as a separate process, so every
profile gets its own host; the host finds its profile from the `.parentlock`
file the browser keeps open and names it from `profiles.ini`. Chromium runs
all profiles in one process and does not tell the host which one it serves,
so Chromium rows show no profile.

After the extension switches the tab and focuses its window, the back end
brings that browser process to the front with `NSRunningApplication`, by pid,
so the right Firefox instance wins when several are open.

Site icons come from `favicons.sqlite` (Firefox) and `Favicons` (Chromium),
read with `/usr/bin/sqlite3` in immutable mode so a locked database never
blocks. Icons are cached in `~/Library/Caches/browvitab/favicons`, and sites
without one are retried after a week. Nothing is downloaded.

Page titles go through Alfred UI's placeholder processing, so anything that
looks like `{file:` or `{var:` gets a zero-width space after the brace.

The binary depends on `serde`, `serde_json` and `libc`.

## Manifests

| Browser | Directory under `~/Library/Application Support` |
| --- | --- |
| Firefox, Zen, Floorp, Waterfox, LibreWolf | `Mozilla/NativeMessagingHosts` |
| LibreWolf | also `librewolf/NativeMessagingHosts` |
| Chrome, Chrome Beta, Dev, Canary | `Google/Chrome*/NativeMessagingHosts` |
| Brave | `BraveSoftware/Brave-Browser/NativeMessagingHosts` |
| Chromium, Edge, Vivaldi, Arc, Thorium, Helium | `<browser>/NativeMessagingHosts` |

Setup writes a manifest only for browsers whose directory exists, and copies
the binary to `~/Library/Application Support/browvitab/bin/` so the manifests
keep working if your Alfred preferences live in a synced folder that moves.

A manifest Browvitab did not write (Vicinae's, a symbolic link, anything
else) is left alone unless you pass `--force`, and uninstall only removes its
own. New manifests are published with a hard link, so a manifest that
appears in the meantime is never overwritten.

## Tests

```sh
cargo test --release
```

The tests cover the host protocol, watch and its cleanup when clients go
away, sockets and stale-socket removal, ordering and ranking, manifest
ownership (Vicinae and symbolic links kept), favicon lookups and the macOS
helpers.

`tests/fake_extension.py` starts the binary the way a browser does and plays
the extension's side, with as many tabs as you like:

```sh
python3 tests/fake_extension.py target/release/browvitab --name Brave --count 300 &
target/release/browvitab list
```

## Tested with

Alfred 6.0 (build 3219) on macOS 27, Firefox with Vicinae extension 1.2.0,
and fake browsers from the script above.

| Check | Result |
| --- | --- |
| Opens from the external trigger and the `alfred://runtrigger` link, with a search | Works |
| Typing filters without starting a process | Works |
| <kbd>↩</kbd> switches the tab and raises Firefox from another app | Works |
| <kbd>⌘</kbd><kbd>↩</kbd> copies the URL without touching the browser | Works |
| <kbd>⌃</kbd><kbd>↩</kbd> closes and selects the next row, repeatedly | Works |
| <kbd>⌘</kbd><kbd>K</kbd> menu and its items | Works |
| The list refreshes while open and keeps the selection | Works |
| No watcher left after the window closes | Works |
| Several browsers in one list | Works |
| A second real Firefox profile | Not tried; labels are unit tested |
| Light and dark mode | Not inspected visually |

Closing the last row that matches the search made Alfred dismiss the window
in one run.

## Design choices

The list has no section header rows. In a `dataview` a header would be a row
that the arrow keys and <kbd>↩</kbd> land on, which would also break "press
<kbd>↩</kbd> right away to go back to the previous tab". The order still
groups pinned tabs and then each browser, and each row shows its browser.

The Script Filter does not cache. The tab list changes all the time and the
back end answers in a few milliseconds, so a cache could only show tabs that
are already closed.

Alfred shows one window at a time, so a single pid file is enough for the
watcher.
