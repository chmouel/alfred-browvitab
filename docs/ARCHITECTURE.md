# How Browvitab works

Browvitab reuses the browser side of [Raffi](https://github.com/chmouel/raffi):
the Vicinae extension talks to a native messaging host, one host per running
browser, and the host serves the tab list over a Unix socket. This document
covers the Alfred 6 side, the macOS port and how the pieces were validated.

| Path | What |
| --- | --- |
| `src/host.rs` | Native messaging host and socket client, ported from Raffi `src/browser_host.rs` |
| `src/tabs.rs` | Ordering and ranking, ported from Raffi `src/ui/wayland/browser_tabs.rs` |
| `src/alfred.rs` | Alfred UI data, JSON Actions and Script Filter JSON |
| `src/favicons.rs` | Site icons read from the browsers' own favicon databases |
| `src/install.rs` | Native messaging manifest registration on macOS |
| `src/macos.rs` | libproc lookups and per-process app activation |
| `workflow/tabs.alfredui/` | The Alfred UI package |
| `scripts/build.py` | Builds the binary, writes `info.plist`, packages the workflow |
| `tests/fake_extension.py` | Stand-in for the browser extension, for tests |

## How it uses Alfred 6

* **Alfred UI object, inside Alfred, stacked.** Keyword, hotkey and an external
  trigger (`tabs`) all open `tabs.alfredui`; the UI's `finish` passes the tab id
  and a `tabaction` variable to a Run Script that switches tabs.
* **Layouts.** Border layouts for the window (search bar / list / footer) and the
  cell; inline layouts for text, badges and key hints.
* **`dataview` with the `basic` provider and `basic` filter.** The search field's
  `changed` event calls `filterdata`, so every keystroke filters natively without
  starting a process. The filter matches a hidden `match` field (title, host,
  URL split into words, full URL) with `wordmatchanyorder`, so "pipe pull"
  finds `…/pipelines-as-code/pull/…`.
* **Ranking outside the UI, off the keystroke path.** The native filter keeps the
  data order. When typing stops (`typingstopped`, async script), the back end
  re-sorts by match quality (title prefix > site > word start > URL; a pasted URL
  puts that site first) and selects the best match, unless you already moved the
  selection (`navigated` variable).
* **Stable uids** `<host pid>:<tab id>` drive reselection: data replacements use
  `select` with `{value:tabs.selected.uid}` read when the actions run, so a
  refresh never moves the selection.
* **Templates** for the cell badges (`badge`) and footer key hints (`keyhint`);
  shared actions live in `actions/*.json` and are included with `{file:}`.
* **Native menus.** The cell `menu` (right-click) and a `popupmenu` JSON Action on
  ⌘K / the ⋯ button.
* **JSON Actions** do everything that needs no process: `copy`, `finish`,
  `update`, `functions` (`selectup`, `filterdata`, …), `popupmenu`.
* **Async script actions** for close, rank and setup.
* **Pulse as an event-driven long poll** (see below).
* **Appearance.** Only system and Alfred theme colours (`labelColor`,
  `secondaryLabelColor`, `alfredSelectedResultBackgroundColor`, …), so light,
  dark and the user's theme apply. The window keeps Alfred's blur; the buttons can
  use the macOS 26 `glass` effect (Workflow Configuration → *Button style*).

### Live updates

Raffi's host already receives `browser/tabs-changed` events. This port adds a
change counter (`generation`) to the host and a `watch` request that blocks
until it moves. The UI's pulse runs `raffi-tabs ui-watch <fingerprint> <query>`:

1. List the hosts (sub-millisecond). If the state differs from the fingerprint
   the UI holds, print one `update` (new data, same filter, selection kept) and
   exit.
2. Otherwise send `watch` to every host and wait. Wake on the first host change,
   on a browser connecting or leaving (socket directory), when the UI quits, or
   after 25 s, and exit with an empty JSON object; the pulse restarts 100 ms later
   with fresh arguments and goes to step 1.

So while the UI is open there is no enumeration loop, only one blocked process
waiting on the extension's own events; a change reaches the screen in about
200 ms. The UI's `quit` event removes the watcher's pid file and the watcher
exits within half a second, so nothing keeps running after the UI closes. No
permanent service exists: the only long-lived processes are the hosts the
browsers start, exactly as with Raffi.

Closing a tab (⌃↩) asks the host, which drops the tab from its list at once,
and answers with the new data and the neighbouring row (the next visible row,
or the previous one at the end) selected, so ⌃↩ can close several tabs in a row.

## macOS port of the host

Kept from Raffi: the Vicinae extension and its protocol
(`browser/init`, `browser/tabs-changed`, `browser/focus-tab`,
`browser/close-tab`), one host process and Unix socket per browser instance
(`/tmp/raffi-<uid>/browsers/<pid>.sock`, private directory), JSON-line requests,
monotonic `expires` deadlines so late requests are never carried out, the
500 ms back-pressure check before writing to the browser, client limits, timeouts,
and stale socket cleanup.

Changed for macOS:

* `/proc` lookups are replaced with libproc (`proc_pidpath`, `proc_pidinfo`); the
  browser is identified from its `.app` bundle (Firefox, Brave, Chrome…).
* Stale sockets are removed only when their pid is gone (macOS refuses
  connections at once when a backlog is full, so a refusal alone is not proof).
* Profiles: Gecko hosts find their profile from the `.parentlock` the browser
  holds open, and name it from `profiles.ini`. Each Firefox profile runs as its
  own process with its own host, so tabs never mix and the uid keeps them apart.
  Chromium runs every profile in one process and does not tell the host which
  profile it serves, so no profile is shown there.
* Window raising: after the extension activates the tab and focuses its window,
  the back end activates the browser **process** with
  `NSRunningApplication` (by pid, so the right Firefox instance comes forward,
  not just any `Firefox.app`).
* Favicons come only from the browsers' own databases (`favicons.sqlite`,
  `Favicons`), read through `/usr/bin/sqlite3` with `immutable=1`; there is no
  network access. Icons are cached in `~/Library/Caches/raffi-alfred/favicons`,
  misses are retried after a week.
* Requests to the extension carry an `id`. Vicinae extension 1.2 drops
  `focus-tab` and `close-tab` requests without one, which Raffi's host omits.
* Page titles are untrusted input for Alfred UI's placeholder preprocessing, so
  anything that looks like `{file:`/`{var:`… is defused with a zero-width space.

No second extension, no WebSocket server, no extra dependencies: the binary
depends on `serde`, `serde_json` and `libc` only.

## Performance

Measured once on this machine (Apple Silicon, macOS 27, Alfred 6.0 b3219):
the back end lists, orders and renders 9 real tabs in 0.2 ms and 513 tabs
(Firefox plus two fake browsers) in 2.6 ms (294 KB of data, 12 MB peak RSS);
a host uses 7–9 MB. Alfred logged 67 ms from *Initialising Alfred UI* to
*did launch* with 513 tabs, preload included. The first opening after new sites
appear spends up to ~120 ms on favicon lookups; later openings use the cache.
`raffi-tabs bench 50` measures your setup.

## Tests

```sh
cargo test --release          # host protocol, watch, sockets, ordering, ranking,
                              # manifests (Vicinae kept), favicons, macOS helpers
python3 tests/fake_extension.py target/release/raffi-tabs --name Brave --count 300 &
target/release/raffi-tabs list
```

## Validation

Done on this machine with Alfred 6.0 (b3219), Firefox with the Vicinae
extension 1.2.0, and two fake browsers from `tests/fake_extension.py`
(driving the UI with synthetic key events and the UI log):

| Check | Result |
| --- | --- |
| Opens from the external trigger; keyword and hotkey use the same object | ✅ trigger; keyword/hotkey wired, not driven |
| Search field focused on open, typing filters via `filterdata` (no process per key) | ✅ |
| ↑/↓, ⌘↑/⌘↓ through root `shortcuts` | ✅ |
| ↩ (text field `actioned` → `finish` → Run Script) switches tab and raises Firefox from another app | ✅ |
| ⌘↩ copies the URL with the `copy` JSON Action, browser untouched | ✅ |
| ⌃↩ closes, keeps the UI open, selects the neighbour; repeated closes | ✅ (fake tabs) |
| **⌥↩** | ❌ never reaches Alfred UI shortcuts: the search field consumes it (no key event logged), so close uses ⌃↩ like Raffi |
| ⌘K `popupmenu` and choosing an item (Copy Title) | ✅ |
| Live refresh while open (new tab, renamed tab), selection kept | ✅ ~200 ms |
| Watcher gone after the UI closes | ✅ |
| Firefox profile detected (`default-esr`), multiple instances labelled | ✅ detection; ✅ labels in unit tests; a second real profile was not started |
| Several browsers at once | ✅ Firefox + 2 fake hosts |
| Light / dark rendering | ⚠️ not seen: no screen-recording permission; only system and theme colours are used |
| Vertical tabs | n/a: the extension reports tabs the same way; nothing to do |

Closing the last row that matches the query made Alfred dismiss the UI in one
run; closing with other matches left keeps it open.

## Design notes and limits


* **No section header rows.** The spec mock-up shows "PINNED / FIREFOX" headers.
  In a `dataview` those would be selectable rows that ↑/↓ and ↩ land on, which
  breaks the keyboard flow (and Raffi's "↩ right away switches back"). The order
  is Raffi's grouped order; the group is visible from the pin badge and the
  browser icon and label on each row instead.
* **Script Filter caching is off.** Tabs change constantly and the back end
  answers in milliseconds, so a cache could only show closed tabs.
* The build targets the host architecture (Homebrew Rust has no cross targets);
  build on Intel for Intel.
* Only one tabs UI runs at a time (Alfred shows one window), so the watcher uses a
  single pid file.
