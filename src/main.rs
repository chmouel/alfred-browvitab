//! raffi-tabs: browser tab switcher for Alfred 6.
//!
//! One binary is the browser's native messaging host (started by the
//! browser, detected from its arguments) and the command-line back end for
//! the Alfred UI and the Script Filter.

mod alfred;
mod favicons;
mod host;
mod install;
mod macos;
mod tabs;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use alfred::{Select, UiState};
use host::{Connected, Request};
use tabs::TabView;

const LIST_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long one live-refresh watch waits before handing back to the pulse.
const WATCH_WINDOW: Duration = Duration::from_secs(25);

const USAGE: &str = "\
raffi-tabs — browser tabs for Alfred 6 (Vicinae extension host)

Usage:
  raffi-tabs list                     Print connected browsers and tabs
  raffi-tabs filter [QUERY]           Script Filter JSON
  raffi-tabs focus UID                Switch to a tab and raise its window
  raffi-tabs close UID                Close a tab
  raffi-tabs url UID                  Print a tab's URL
  raffi-tabs install [--force]        Install the host and browser manifests
  raffi-tabs uninstall                Remove what install added
  raffi-tabs status                   Show manifests and connected hosts
  raffi-tabs bench [N]                Time N list+render rounds
Alfred UI back end:
  ui-load [QUERY] | ui-watch FINGERPRINT [QUERY] | ui-rank QUERY [NAVIGATED]
  ui-close UID [QUERY] | ui-setup | ui-quit
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if host::is_native_host_invocation(&args) {
        return ExitCode::from(host::run_native_host(&args) as u8);
    }
    trace(&args);
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let result = match arg(1) {
        "list" => cmd_list(),
        "filter" => cmd_filter(arg(2)),
        "focus" => cmd_focus(arg(2)),
        "close" => cmd_close(arg(2)),
        "url" => cmd_url(arg(2)),
        "install" => cmd_install(args.iter().any(|a| a == "--force")),
        "uninstall" => cmd_uninstall(),
        "status" => cmd_status(),
        "bench" => cmd_bench(arg(2).parse().unwrap_or(20)),
        "ui-load" => ui_load(arg(2)),
        "ui-watch" => ui_watch(arg(2), arg(3)),
        "ui-rank" => ui_rank(arg(2), arg(3) == "1"),
        "ui-close" => ui_close(arg(2), arg(3)),
        "ui-setup" => ui_setup(),
        "ui-quit" => ui_quit(),
        // Traced no-op, for inspecting UI state while troubleshooting.
        "ui-debug" => print_json(&json!({})),
        "" | "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("raffi-tabs: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Troubleshooting: when `/tmp/raffi-<uid>/trace` exists, log every
/// invocation there (Alfred UI scripts get no visible stderr).
fn trace(args: &[String]) {
    let Ok(dir) = host::socket_dir() else { return };
    let path = dir.with_file_name("trace");
    if !path.exists() {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new().append(true).open(&path) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let _ = writeln!(file, "{now:.3} pid={} {:?}", std::process::id(), &args[1..]);
    }
}

fn socket_dir() -> Result<PathBuf, String> {
    host::socket_dir().map_err(|e| format!("socket directory: {e}"))
}

fn connected() -> Result<Vec<Connected>, String> {
    Ok(host::list_browsers(&socket_dir()?, LIST_TIMEOUT))
}

/// Resolve `<host pid>:<tab id>` to the host socket and tab id.
fn parse_uid(uid: &str) -> Result<(PathBuf, i64), String> {
    // An unresolved `{value:…}` means nothing was selected.
    if uid.is_empty() || uid.contains('{') {
        return Err("no tab selected".into());
    }
    let (pid, tab) = uid
        .split_once(':')
        .ok_or_else(|| format!("invalid tab id {uid:?}"))?;
    let pid: u32 = pid.parse().map_err(|_| format!("invalid tab id {uid:?}"))?;
    let tab: i64 = tab.parse().map_err(|_| format!("invalid tab id {uid:?}"))?;
    Ok((socket_dir()?.join(format!("{pid}.sock")), tab))
}

fn print_json(value: &Value) -> Result<(), String> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())
}

fn cmd_list() -> Result<(), String> {
    let browsers = connected()?;
    let mut out = io::stdout().lock();
    for b in &browsers {
        let br = &b.listing.browser;
        let _ = writeln!(
            out,
            "{} [{}] pid={:?} profile={:?} app={:?} tabs={} socket={}",
            br.name,
            br.id,
            br.pid,
            br.profile,
            br.app,
            b.listing.tabs.len(),
            b.socket.display()
        );
    }
    for tab in tabs::order(&browsers) {
        let _ = writeln!(
            out,
            "{:<10} {:<12} {}  —  {}",
            tab.uid, tab.group, tab.title, tab.url
        );
    }
    Ok(())
}

fn setup_hint() -> Option<String> {
    let support = install::support_dir().ok()?;
    let status = install::status(&support);
    if status.is_empty() {
        return Some("No supported browser found".into());
    }
    if status
        .iter()
        .any(|(_, _, owner)| matches!(owner, Some(install::Owner::Ours(_))))
    {
        return Some(
            "Waiting for the Vicinae extension to connect (it retries every minute)".into(),
        );
    }
    if status
        .iter()
        .any(|(_, _, owner)| owner == &Some(install::Owner::Vicinae))
    {
        return Some(
            "The browsers talk to Vicinae; run the installer with --force to switch".into(),
        );
    }
    Some("Host not installed: type “tabs:setup” or run install.sh".into())
}

fn cmd_filter(_query: &str) -> Result<(), String> {
    let browsers = connected()?;
    let ordered = tabs::order(&browsers);
    let icons = favicons::resolve(&ordered);
    let hint = if browsers.is_empty() {
        setup_hint()
    } else {
        None
    };
    print_json(&alfred::script_filter(&ordered, &icons, hint.as_deref()))
}

fn browser_of(socket: &Path) -> Option<host::Browser> {
    host::request_before(socket, &Request::List, Instant::now() + LIST_TIMEOUT)
        .ok()?
        .listing
        .map(|l| l.browser)
}

fn cmd_focus(uid: &str) -> Result<(), String> {
    if uid.is_empty() || uid.contains('{') {
        return Ok(());
    }
    let (socket, tab_id) = parse_uid(uid)?;
    let browser = browser_of(&socket);
    host::send_request(&socket, &Request::Focus { tab_id })?;
    // The extension raises the window inside the browser; bring that
    // browser process (not just any instance of the app) to the front.
    if let Some(pid) = browser.and_then(|b| b.pid) {
        for _ in 0..4 {
            if macos::frontmost_pid() == Some(pid) {
                break;
            }
            macos::activate_pid(pid);
            thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

fn cmd_close(uid: &str) -> Result<(), String> {
    let (socket, tab_id) = parse_uid(uid)?;
    host::send_request(&socket, &Request::Close { tab_id })
}

fn cmd_url(uid: &str) -> Result<(), String> {
    let (socket, tab_id) = parse_uid(uid)?;
    let response = host::request_before(&socket, &Request::List, Instant::now() + LIST_TIMEOUT)
        .map_err(|e| e.to_string())?;
    let tab = response
        .listing
        .and_then(|l| l.tabs.into_iter().find(|t| t.id == tab_id))
        .ok_or("the tab is no longer open")?;
    print!("{}", tab.url);
    Ok(())
}

fn print_reports(reports: &[install::Report]) {
    for report in reports {
        let what = match &report.outcome {
            install::Outcome::Written => "installed".to_string(),
            install::Outcome::Unchanged => "already installed".to_string(),
            install::Outcome::Kept(reason) => {
                format!("left alone: {reason} (use --force to replace)")
            }
            install::Outcome::Removed => "removed".to_string(),
            install::Outcome::Failed(error) => format!("failed: {error}"),
        };
        println!(
            "{}: {} — {what}",
            report.browsers.join(", "),
            report.path.display()
        );
    }
}

fn install_now(force: bool) -> Result<(PathBuf, Vec<install::Report>), String> {
    let support = install::support_dir().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = if std::env::var_os("RAFFI_TABS_NO_COPY").is_some() {
        exe
    } else {
        install::install_binary(&support, &exe).map_err(|e| format!("cannot copy the host: {e}"))?
    };
    let reports = install::install_manifests(&support, &target, force);
    Ok((target, reports))
}

fn cmd_install(force: bool) -> Result<(), String> {
    let (target, reports) = install_now(force)?;
    println!("Host: {}", target.display());
    if reports.is_empty() {
        println!("No supported browser found under ~/Library/Application Support.");
    }
    print_reports(&reports);
    println!(
        "The extension reconnects on its own within a minute; toggle it off and on to connect now."
    );
    Ok(())
}

fn cmd_uninstall() -> Result<(), String> {
    let support = install::support_dir().map_err(|e| e.to_string())?;
    let reports = install::uninstall_manifests(&support);
    if reports.is_empty() {
        println!("No manifest of ours to remove.");
    }
    print_reports(&reports);
    let bin = install::install_dir(&support);
    if bin.exists() {
        fs::remove_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
        println!("Removed {}", bin.display());
    }
    if let Some(cache) = favicons::cache_dir().and_then(|d| d.parent().map(Path::to_path_buf)) {
        let _ = fs::remove_dir_all(cache);
    }
    Ok(())
}

fn cmd_status() -> Result<(), String> {
    let support = install::support_dir().map_err(|e| e.to_string())?;
    for (browsers, path, owner) in install::status(&support) {
        let who = match owner {
            None => "no manifest".to_string(),
            Some(install::Owner::Ours(p)) => format!(
                "raffi-tabs at {}{}",
                p.display(),
                if p.exists() { "" } else { " (missing!)" }
            ),
            Some(install::Owner::Vicinae) => "Vicinae".to_string(),
            Some(install::Owner::Other(r)) => format!("other: {r}"),
        };
        println!("{}: {} — {who}", browsers.join(", "), path.display());
    }
    let browsers = connected()?;
    println!("Connected hosts: {}", browsers.len());
    for b in browsers {
        let br = &b.listing.browser;
        println!(
            "  {} (pid {:?}, profile {:?}): {} tabs",
            br.name,
            br.pid,
            br.profile,
            b.listing.tabs.len()
        );
    }
    Ok(())
}

fn ui_state(browsers: &[Connected], ordered: &[TabView]) -> UiState {
    let icons = favicons::resolve(ordered);
    let data: Vec<Value> = ordered
        .iter()
        .map(|t| alfred::ui_entry(t, &icons))
        .collect();
    let fingerprint = alfred::fingerprint(&data);
    let mut names: Vec<String> = Vec::new();
    let mut labels: Vec<String> = ordered.iter().map(|t| t.source.clone()).collect();
    labels.extend(browsers.iter().map(|b| b.listing.browser.name.clone()));
    let mut seen = std::collections::HashSet::new();
    for label in labels {
        if !label.is_empty() && seen.insert(label.clone()) {
            names.push(label);
        }
    }
    // Several instances listed per profile already; drop bare names then.
    let profiled: Vec<String> = names
        .iter()
        .filter(|n| n.contains(" · "))
        .cloned()
        .collect();
    names.retain(|n| {
        n.contains(" · ") || !profiled.iter().any(|p| p.starts_with(&format!("{n} · ")))
    });
    UiState {
        data,
        fingerprint,
        tab_count: ordered.len(),
        browsers: names,
        setup_needed: browsers.is_empty(),
    }
}

fn placeholder(browsers: &[Connected]) -> String {
    if browsers.is_empty() {
        setup_hint().unwrap_or_else(|| "No browser connected".into())
    } else {
        "No matching tabs".into()
    }
}

/// Preload: fill the list before the UI shows. The first row is Raffi's
/// "previous tab", so ↩ right away switches back.
fn ui_load(query: &str) -> Result<(), String> {
    let browsers = connected()?;
    let ordered = tabs::rank(tabs::order(&browsers), query);
    let state = ui_state(&browsers, &ordered);
    let select = ordered
        .first()
        .map(|t| Select::Uid(t.uid.clone()))
        .unwrap_or(Select::First);
    let mut out = alfred::ui_update(&state, select, &placeholder(&browsers));
    // Unranked state, so the watcher compares like with like.
    if !query.trim().is_empty() {
        let plain = ui_state(&browsers, &tabs::order(&browsers));
        out["variables"]["fingerprint"] = json!(plain.fingerprint);
    }
    print_json(&out)
}

/// Reorder for the query once typing pauses. The native filter has
/// already narrowed the list on every keystroke.
fn ui_rank(query: &str, navigated: bool) -> Result<(), String> {
    let browsers = connected()?;
    let plain = tabs::order(&browsers);
    let fingerprint = ui_state(&browsers, &plain).fingerprint;
    let ranked = tabs::rank(plain, query);
    let state = ui_state(&browsers, &ranked);
    let select = if navigated || query.trim().is_empty() {
        Select::Current
    } else {
        Select::First
    };
    let mut out = alfred::ui_update(&state, select, &placeholder(&browsers));
    out["variables"]["fingerprint"] = json!(fingerprint);
    print_json(&out)
}

fn watcher_file() -> Result<PathBuf, String> {
    Ok(socket_dir()?.with_file_name("alfred-ui-watch.pid"))
}

/// The pulse: report changes to the open UI, waiting on the hosts' own
/// change events in between (no enumeration while nothing happens).
///
/// If the browsers differ from `fingerprint` now, print the update. Else
/// block until a host's generation moves, a browser connects or leaves,
/// the UI quits, or the window ends, and exit without output; the pulse
/// restarts at once with fresh arguments (the query may have changed) and
/// takes the first branch.
fn ui_watch(fingerprint: &str, query: &str) -> Result<(), String> {
    let dir = socket_dir()?;
    let me = std::process::id();
    let pidfile = watcher_file()?;
    let _ = fs::write(&pidfile, me.to_string());

    let browsers = host::list_browsers(&dir, LIST_TIMEOUT);
    let plain = tabs::order(&browsers);
    let current = ui_state(&browsers, &plain);
    if current.fingerprint != fingerprint {
        let ranked = tabs::rank(plain, query);
        let mut state = ui_state(&browsers, &ranked);
        state.fingerprint = current.fingerprint;
        return print_json(&alfred::ui_update(
            &state,
            Select::Current,
            &placeholder(&browsers),
        ));
    }

    let (tx, rx) = mpsc::channel::<()>();
    for b in &browsers {
        let tx = tx.clone();
        let socket = b.socket.clone();
        let since = b.generation;
        thread::spawn(move || {
            let request = Request::Watch {
                since,
                timeout_ms: WATCH_WINDOW.as_millis() as u64,
            };
            let deadline = Instant::now() + WATCH_WINDOW + Duration::from_secs(2);
            match host::request_before(&socket, &request, deadline) {
                Ok(r) if r.generation.is_some_and(|g| g != since) => {
                    let _ = tx.send(());
                }
                // Host gone: its socket vanishes, the directory check sees it.
                _ => {}
            }
        });
    }
    let sockets = host::host_sockets(&dir);
    let started = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        let still_ours =
            fs::read_to_string(&pidfile).is_ok_and(|text| text.trim() == me.to_string());
        if !still_ours || started.elapsed() >= WATCH_WINDOW || host::host_sockets(&dir) != sockets {
            break;
        }
    }
    // Exiting drops the waiting threads; nothing else to clean up.
    let _ = print_json(&json!({}));
    io::stdout().flush().ok();
    std::process::exit(0)
}

/// Close the tab and redraw at once, keeping the list open with the
/// neighbouring row selected so ⌥↩ can close several tabs in a row.
fn ui_close(uid: &str, query: &str) -> Result<(), String> {
    let dir = socket_dir()?;
    let before = tabs::rank(tabs::order(&host::list_browsers(&dir, LIST_TIMEOUT)), query);
    let visible: Vec<&TabView> = before.iter().filter(|t| tabs::matches(t, query)).collect();
    let next = visible.iter().position(|t| t.uid == uid).and_then(|i| {
        visible
            .get(i + 1)
            .or_else(|| i.checked_sub(1).and_then(|j| visible.get(j)))
            .map(|t| t.uid.clone())
    });
    if let Err(error) = cmd_close(uid) {
        return print_json(&json!({
            "update": { "ids": { "status": { "text": format!("Could not close: {error}") } } },
            "sound": { "src": "Funk" }
        }));
    }
    // The host drops the tab from its list as soon as it asks the browser.
    let browsers = host::list_browsers(&dir, LIST_TIMEOUT);
    let plain = tabs::order(&browsers);
    let fingerprint = ui_state(&browsers, &plain).fingerprint;
    let state = ui_state(&browsers, &tabs::rank(plain, query));
    let select = next.map(Select::Uid).unwrap_or(Select::First);
    let mut out = alfred::ui_update(&state, select, &placeholder(&browsers));
    out["variables"]["fingerprint"] = json!(fingerprint);
    print_json(&out)
}

fn ui_setup() -> Result<(), String> {
    let text = match install_now(false) {
        Ok((_, reports)) => {
            let kept: Vec<String> = reports
                .iter()
                .filter_map(|r| match &r.outcome {
                    install::Outcome::Kept(why) => Some(format!("{}: {why}", r.browsers.join("/"))),
                    _ => None,
                })
                .collect();
            if reports.is_empty() {
                "No supported browser found".to_string()
            } else if !kept.is_empty() {
                format!(
                    "Not replaced — {} (run install.sh --force)",
                    kept.join("; ")
                )
            } else {
                "Host installed. Toggle the Vicinae extension or wait a minute…".to_string()
            }
        }
        Err(error) => format!("Setup failed: {error}"),
    };
    print_json(&json!({
        "update": { "ids": { "status": { "text": text } } },
        "pulse": { "now": true }
    }))
}

/// The UI is closing: release the watcher, which exits within half a
/// second when its pid file no longer names it.
fn ui_quit() -> Result<(), String> {
    let pidfile = watcher_file()?;
    let _ = fs::remove_file(pidfile);
    print_json(&json!({}))
}

fn cmd_bench(rounds: usize) -> Result<(), String> {
    let mut times = Vec::new();
    let mut size = 0;
    let mut count = 0;
    for _ in 0..rounds.max(1) {
        let start = Instant::now();
        let browsers = connected()?;
        let ordered = tabs::order(&browsers);
        let state = ui_state(&browsers, &ordered);
        let out = alfred::ui_update(&state, Select::First, "");
        size = serde_json::to_vec(&out).map(|v| v.len()).unwrap_or(0);
        count = ordered.len();
        times.push(start.elapsed());
    }
    times.sort();
    println!(
        "tabs={count} json={}KB  min={:?} median={:?} max={:?}",
        size / 1024,
        times[0],
        times[times.len() / 2],
        times[times.len() - 1]
    );
    Ok(())
}
