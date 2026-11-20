//! Native messaging host for the Vicinae browser extension.
//!
//! The browser starts this program as the `com.vicinae.vicinae` host when
//! the extension loads. The host keeps the tab list the extension reports
//! (`browser/tabs-changed`) and serves it over a Unix socket, one per
//! running browser instance. Clients ask the host to focus or close a tab,
//! and the host passes that on to the extension. A `generation` counter and
//! a blocking `watch` request let an open Alfred UI refresh on the
//! extension's own events.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::macos;

pub const HOST_NAME: &str = "com.vicinae.vicinae";
pub const CHROME_EXTENSION_ID: &str = "kcmipingpfbohfjckomimmahknoddnke";
pub const FIREFOX_EXTENSION_ID: &str = "firefox@vicinae.com";

/// The extension may send up to 64 MiB; anything bigger is a broken stream.
const MAX_FRAME: u32 = 64 * 1024 * 1024;
const MAX_REQUEST: u64 = 64 * 1024;
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a client request waits for the browser to take a message.
const OUTPUT_WAIT: Duration = Duration::from_millis(500);
/// Watchers hold a connection each, so leave room for them.
const MAX_CLIENTS: usize = 16;
/// Upper bound for one `watch` request.
pub const MAX_WATCH: Duration = Duration::from_secs(60);
/// How often a waiting watch checks whether its client is still there.
const WATCH_SLICE: Duration = Duration::from_millis(250);

/// A browser tab as the extension reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Tab {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub window_id: i64,
    pub active: bool,
    pub audible: bool,
    pub muted: bool,
    /// Milliseconds since the epoch.
    pub last_accessed: f64,
    pub pinned: bool,
}

/// The browser a host serves.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Browser {
    /// Short name such as `firefox` or `google-chrome`.
    pub id: String,
    pub name: String,
    /// The browser process, which owns its windows.
    pub pid: Option<u32>,
    /// The `.app` bundle, for its icon and activation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Profile name, when the browser tells us reliably (Gecko only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Profile directory, used to find stored favicons.
    #[serde(skip_serializing_if = "Option::is_none", rename = "profileDir")]
    pub profile_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Listing {
    pub browser: Browser,
    pub tabs: Vec<Tab>,
}

/// A client request, one JSON line per connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum Request {
    List,
    Focus {
        #[serde(rename = "tabId")]
        tab_id: i64,
    },
    Close {
        #[serde(rename = "tabId")]
        tab_id: i64,
    },
    /// Answer with the listing once the generation differs from `since`,
    /// or after `timeoutMs`.
    Watch {
        since: u64,
        #[serde(rename = "timeoutMs")]
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listing: Option<Listing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::default()
        }
    }
}

/// Whether the browser started this process as a native messaging host.
/// Chrome passes the extension origin first; Firefox passes the manifest
/// path, then the extension ID.
pub fn is_native_host_invocation(args: &[String]) -> bool {
    args.get(1)
        .is_some_and(|arg| arg.starts_with("chrome-extension://"))
        || args.get(2).is_some_and(|arg| arg == FIREFOX_EXTENSION_ID)
}

/// Read one native messaging frame: a native-endian u32 length, then JSON.
/// Returns None at end of input.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut size = [0u8; 4];
    match reader.read_exact(&mut size) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let size = u32::from_ne_bytes(size);
    if size == 0 || size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid message size {size}"),
        ));
    }
    let mut body = vec![0u8; size as usize];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Write one frame in a single write.
pub fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let size = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&size.to_ne_bytes());
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()
}

/// Whether `fd` can take a write before `deadline`.
fn writable_before(fd: RawFd, deadline: Instant) -> bool {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let millis =
            libc::c_int::try_from(left.as_micros().div_ceil(1000)).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd, for the duration of the call.
        match unsafe { libc::poll(&mut poll, 1, millis) } {
            1 => return poll.revents & libc::POLLOUT != 0,
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return false,
        }
    }
}

struct State {
    listing: Listing,
    generation: u64,
}

/// The host state shared by the extension reader and the socket clients.
pub struct Host<W> {
    state: Mutex<State>,
    changed: Condvar,
    out: Mutex<W>,
    out_fd: Option<RawFd>,
    /// Ids for requests to the extension; 1.2+ drops requests without one.
    next_id: AtomicU64,
}

impl<W: Write> Host<W> {
    pub fn new(browser: Browser, out: W) -> Self {
        Self {
            state: Mutex::new(State {
                listing: Listing {
                    browser,
                    tabs: Vec::new(),
                },
                generation: 1,
            }),
            changed: Condvar::new(),
            out: Mutex::new(out),
            out_fd: None,
            next_id: AtomicU64::new(1),
        }
    }

    /// Wait for `fd`, which `out` writes to, to have room before sending
    /// client messages.
    pub fn polling(mut self, fd: RawFd) -> Self {
        self.out_fd = Some(fd);
        self
    }

    /// Change the state and wake the watchers.
    fn mutate(&self, change: impl FnOnce(&mut Listing)) {
        if let Ok(mut state) = self.state.lock() {
            change(&mut state.listing);
            state.generation += 1;
            self.changed.notify_all();
        }
    }

    #[cfg(test)]
    pub fn generation(&self) -> u64 {
        self.state.lock().map(|s| s.generation).unwrap_or(0)
    }

    fn send(&self, message: &Value) -> io::Result<()> {
        let body = serde_json::to_vec(message)?;
        let mut out = self
            .out
            .lock()
            .map_err(|_| io::Error::other("output lock poisoned"))?;
        write_frame(&mut *out, &body)
    }

    /// Send on behalf of a client. When an earlier message is still stuck
    /// because the browser stopped reading, give up instead of queueing
    /// behind it. Nothing is sent once `expires` has passed.
    fn send_for_client(&self, message: &Value, expires: Option<Instant>) -> io::Result<()> {
        let body = serde_json::to_vec(message)?;
        let expired = || expires.is_some_and(|expires| Instant::now() >= expires);
        let give_up = Instant::now() + OUTPUT_WAIT;
        loop {
            match self.out.try_lock() {
                Ok(_) if expired() => return Err(timed_out("the request expired")),
                Ok(mut out) => {
                    let expiring = expires.filter(|expires| *expires <= give_up);
                    let deadline = expiring.unwrap_or(give_up);
                    if self.out_fd.is_some_and(|fd| !writable_before(fd, deadline)) {
                        return Err(if expiring.is_some() {
                            timed_out("the request expired")
                        } else {
                            io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "the browser is not reading messages",
                            )
                        });
                    }
                    return write_frame(&mut *out, &body);
                }
                Err(TryLockError::WouldBlock) if expired() => {
                    return Err(timed_out("the request expired"))
                }
                Err(TryLockError::WouldBlock) if Instant::now() < give_up => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "the browser is not reading messages",
                    ))
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("output lock poisoned"))
                }
            }
        }
    }

    /// Handle a message from the extension.
    pub fn handle_extension_message(&self, raw: &[u8]) -> io::Result<()> {
        let message: Value = serde_json::from_slice(raw)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to a request; the host sends none.
            return Ok(());
        };
        let id = message.get("id").filter(|id| !id.is_null());
        match method {
            "browser/init" => {
                // Firefox forks report their own name ("LibreWolf", "Zen").
                if let Some(name) = message
                    .pointer("/data/name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty() && !name.contains('/'))
                {
                    let name = name.to_string();
                    self.mutate(|listing| {
                        if listing.browser.pid.is_none() {
                            listing.browser.name = name;
                        }
                    });
                }
                match id {
                    Some(id) => self.send(&json!({ "id": id, "result": {} })),
                    None => Ok(()),
                }
            }
            "browser/tabs-changed" => {
                let data = message.get("data").cloned().unwrap_or(Value::Null);
                match serde_json::from_value::<Vec<Tab>>(data) {
                    Ok(tabs) => self.mutate(|listing| listing.tabs = tabs),
                    Err(error) => eprintln!("browvitab host: invalid tab list: {error}"),
                }
                Ok(())
            }
            _ => match id {
                Some(id) => self.send(&json!({ "id": id, "error": "unsupported method" })),
                None => Ok(()),
            },
        }
    }

    /// Set details found after start-up, such as the profile.
    pub fn update_browser(&self, change: impl FnOnce(&mut Browser)) {
        self.mutate(|listing| change(&mut listing.browser));
    }

    /// Answer a client request.
    #[cfg(test)]
    pub fn handle_request(&self, request: Request) -> Response {
        self.handle_request_until(request, None)
    }

    /// Answer a client request, without acting on it after `expires`.
    #[cfg(test)]
    pub fn handle_request_until(&self, request: Request, expires: Option<Instant>) -> Response {
        self.handle_request_cancellable(request, expires, &|| false)
    }

    /// As `handle_request_until`; a `watch` also ends as soon as
    /// `cancelled` says the client went away, so abandoned watches do not
    /// hold client slots.
    pub fn handle_request_cancellable(
        &self,
        request: Request,
        expires: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Response {
        let Ok(state) = self.state.lock() else {
            return Response::error("host state is unavailable");
        };
        let (method, tab_id) = match request {
            Request::List => {
                return Response {
                    listing: Some(state.listing.clone()),
                    generation: Some(state.generation),
                    error: None,
                }
            }
            Request::Watch { since, timeout_ms } => {
                let wait = Duration::from_millis(timeout_ms).min(MAX_WATCH);
                let wait = match expires {
                    Some(expires) => wait.min(expires.saturating_duration_since(Instant::now())),
                    None => wait,
                };
                let end = Instant::now() + wait;
                let mut state = state;
                while state.generation == since {
                    let left = end.saturating_duration_since(Instant::now());
                    if left.is_zero() || cancelled() {
                        break;
                    }
                    let Ok((next, _)) =
                        self.changed
                            .wait_timeout_while(state, left.min(WATCH_SLICE), |s| {
                                s.generation == since
                            })
                    else {
                        return Response::error("host state is unavailable");
                    };
                    state = next;
                }
                return Response {
                    listing: Some(state.listing.clone()),
                    generation: Some(state.generation),
                    error: None,
                };
            }
            Request::Focus { tab_id } => ("browser/focus-tab", tab_id),
            Request::Close { tab_id } => ("browser/close-tab", tab_id),
        };
        if !state.listing.tabs.iter().any(|tab| tab.id == tab_id) {
            return Response::error("the tab is no longer open");
        }
        drop(state);
        if let Err(err) = self.send_for_client(
            &json!({
                "id": format!("browvitab-{}", self.next_id.fetch_add(1, Ordering::SeqCst)),
                "method": method,
                "data": { "tabId": tab_id }
            }),
            expires,
        ) {
            return Response::error(format!("cannot reach the browser: {err}"));
        }
        if method == "browser/close-tab" {
            // Gone right away, so a list fetched before the extension's next
            // update does not show it.
            self.mutate(|listing| listing.tabs.retain(|tab| tab.id != tab_id));
        }
        Response::default()
    }

    /// Serve one client connection: a request line, then a response line.
    pub fn serve_client(&self, stream: UnixStream) -> io::Result<()> {
        stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
        stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
        let mut line = String::new();
        BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line)?;
        let parsed = serde_json::from_str::<Value>(line.trim()).and_then(|value| {
            let expires = value.get("expires").and_then(Value::as_u64);
            serde_json::from_value::<Request>(value).map(|request| (request, expires))
        });
        let fd = stream.as_raw_fd();
        let cancelled = move || peer_closed(fd);
        let response = match parsed {
            Ok((request, None)) => self.handle_request_cancellable(request, None, &cancelled),
            // The client stops waiting at `expires`; acting later would
            // surprise it.
            Ok((request, Some(expires))) => match monotonic_millis() {
                Ok(now) if expires > now => self.handle_request_cancellable(
                    request,
                    Some(Instant::now() + Duration::from_millis(expires - now)),
                    &cancelled,
                ),
                Ok(_) => Response::error("the request expired"),
                Err(clock) => Response::error(format!("cannot read the clock: {clock}")),
            },
            Err(error) => Response::error(format!("invalid request: {error}")),
        };
        let mut body = serde_json::to_vec(&response)?;
        body.push(b'\n');
        (&stream).write_all(&body)
    }
}

/// Whether the peer of a connected socket has hung up: readable with
/// nothing to read. Clients send nothing after their request line.
fn peer_closed(fd: RawFd) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd; zero timeout.
    if unsafe { libc::poll(&mut poll, 1, 0) } != 1 {
        return false;
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        return true;
    }
    let mut byte = 0u8;
    // SAFETY: peeks at most one byte into a valid buffer.
    let read = unsafe {
        libc::recv(
            fd,
            std::ptr::addr_of_mut!(byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    read == 0
}

/// Milliseconds on the monotonic clock, which all processes on the machine
/// share and which changes to the wall clock do not move.
fn monotonic_millis() -> io::Result<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec for the call to fill in.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(now.tv_sec as u64 * 1000 + now.tv_nsec as u64 / 1_000_000)
}

struct ClientSlot(Arc<AtomicUsize>);

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accept client connections, each on its own thread, at most `max` at a
/// time. Connections past the limit are closed right away.
pub fn serve_clients<W: Write + Send + 'static>(
    listener: UnixListener,
    host: Arc<Host<W>>,
    max: usize,
) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        let busy = active.fetch_add(1, Ordering::SeqCst) >= max;
        let slot = ClientSlot(Arc::clone(&active));
        if busy {
            eprintln!("browvitab host: too many clients, dropping one");
            continue;
        }
        let host = Arc::clone(&host);
        let spawned = thread::Builder::new()
            .name("browvitab-client".to_string())
            .spawn(move || {
                let _slot = slot;
                if let Err(error) = host.serve_client(stream) {
                    eprintln!("browvitab host: client error: {error}");
                }
            });
        if let Err(error) = spawned {
            eprintln!("browvitab host: cannot serve a client: {error}");
        }
    }
}

fn current_uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// Where the hosts put their sockets: `$BROWVITAB_SOCKET_DIR` for tests,
/// otherwise `/tmp/browvitab-<uid>/browsers`. A fixed path keeps browsers (started by launchd)
/// and Alfred scripts (with their own environment) in agreement, and stays
/// well under the 104-byte socket path limit of macOS.
pub fn socket_dir() -> io::Result<PathBuf> {
    let uid = current_uid();
    if let Some(dir) = std::env::var_os("BROWVITAB_SOCKET_DIR").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        private_dir(&dir, uid)?;
        return Ok(dir);
    }
    let base = PathBuf::from(format!("/tmp/browvitab-{uid}"));
    private_dir(&base, uid)?;
    let dir = base.join("browsers");
    private_dir(&dir, uid)?;
    Ok(dir)
}

/// Create `dir` readable by its owner only, or check that the existing one
/// is a real directory owned by `uid`. Symbolic links are refused.
fn private_dir(dir: &Path, uid: u32) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() || meta.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a directory owned by you", dir.display()),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Name a browser from its macOS executable path.
pub fn classify_browser(exe: &Path) -> Option<(&'static str, &'static str)> {
    let path = exe.to_string_lossy().to_lowercase();
    let bundle = macos::app_bundle(exe)
        .and_then(|b| b.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let name = exe.file_name()?.to_str()?.to_lowercase();
    let key = if bundle.is_empty() { name } else { bundle };
    Some(match key.as_str() {
        k if k.starts_with("firefox developer") => {
            ("firefox-developer", "Firefox Developer Edition")
        }
        k if k.starts_with("firefox nightly") => ("firefox-nightly", "Firefox Nightly"),
        k if k.starts_with("firefox") => ("firefox", "Firefox"),
        "librewolf" => ("librewolf", "LibreWolf"),
        k if k.starts_with("zen") => ("zen", "Zen"),
        "floorp" => ("floorp", "Floorp"),
        "waterfox" => ("waterfox", "Waterfox"),
        k if k.starts_with("brave browser") => ("brave-browser", "Brave"),
        "google chrome beta" => ("google-chrome-beta", "Google Chrome Beta"),
        "google chrome dev" => ("google-chrome-dev", "Google Chrome Dev"),
        "google chrome canary" => ("google-chrome-canary", "Google Chrome Canary"),
        "google chrome" => ("google-chrome", "Google Chrome"),
        "chromium" => ("chromium", "Chromium"),
        "vivaldi" => ("vivaldi", "Vivaldi"),
        k if k.starts_with("microsoft edge") => ("microsoft-edge", "Microsoft Edge"),
        k if k.starts_with("opera") => ("opera", "Opera"),
        "arc" => ("arc", "Arc"),
        "thorium" => ("thorium", "Thorium"),
        "helium" => ("helium", "Helium"),
        _ if path.contains("firefox") => ("firefox", "Firefox"),
        _ => return None,
    })
}

/// Whether the browser uses the Gecko engine (has `.parentlock` profiles).
pub fn is_gecko(id: &str) -> bool {
    matches!(
        id,
        "firefox"
            | "firefox-developer"
            | "firefox-nightly"
            | "librewolf"
            | "zen"
            | "floorp"
            | "waterfox"
    )
}

/// Find the browser among the ancestors of this process, or guess it from
/// how it was started.
pub fn detect_browser(args: &[String]) -> Browser {
    let mut pid = Some(std::os::unix::process::parent_id());
    for _ in 0..4 {
        let Some(current) = pid.filter(|&pid| pid > 1) else {
            break;
        };
        if let Some(exe) = macos::exe_path(current) {
            if let Some((id, name)) = classify_browser(&exe) {
                return Browser {
                    id: id.to_string(),
                    name: name.to_string(),
                    pid: Some(current),
                    app: macos::app_bundle(&exe).map(|p| p.to_string_lossy().into_owned()),
                    ..Browser::default()
                };
            }
        }
        pid = macos::parent_of(current);
    }
    let (id, name) = if args.get(2).is_some_and(|arg| arg == FIREFOX_EXTENSION_ID) {
        ("firefox", "Firefox")
    } else {
        ("chromium", "Chromium")
    };
    Browser {
        id: id.to_string(),
        name: name.to_string(),
        ..Browser::default()
    }
}

/// Run as the native messaging host until the browser closes stdin. Returns
/// the exit code.
pub fn run_native_host(args: &[String]) -> i32 {
    let browser = detect_browser(args);
    let dir = match socket_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("browvitab host: no socket directory: {error}");
            return 1;
        }
    };
    let socket = dir.join(format!("{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "browvitab host: cannot listen on {}: {error}",
                socket.display()
            );
            return 1;
        }
    };
    let _ = fs::set_permissions(&socket, fs::Permissions::from_mode(0o600));
    // Unbuffered, so each frame is one write the poll in send_for_client
    // can vouch for.
    let out = match io::stdout().as_fd().try_clone_to_owned() {
        Ok(out) => fs::File::from(out),
        Err(error) => {
            eprintln!("browvitab host: cannot use the output: {error}");
            let _ = fs::remove_file(&socket);
            return 1;
        }
    };
    let out_fd = out.as_raw_fd();
    let gecko_pid = browser.pid.filter(|_| is_gecko(&browser.id));
    let host = Arc::new(Host::new(browser, out).polling(out_fd));

    // The profile takes an lsof run to find; do it off the start-up path.
    if let Some(pid) = gecko_pid {
        let host = Arc::clone(&host);
        let _ = thread::Builder::new()
            .name("browvitab-profile".to_string())
            .spawn(move || {
                if let Some(dir) = macos::gecko_profile_dir(pid) {
                    let name = macos::gecko_profile_name(&dir);
                    host.update_browser(|browser| {
                        browser.profile = Some(name);
                        browser.profile_dir = Some(dir.to_string_lossy().into_owned());
                    });
                }
            });
    }

    let server = Arc::clone(&host);
    if let Err(error) = thread::Builder::new()
        .name("browvitab-server".to_string())
        .spawn(move || serve_clients(listener, server, MAX_CLIENTS))
    {
        eprintln!("browvitab host: cannot start the socket server: {error}");
        let _ = fs::remove_file(&socket);
        return 1;
    }

    let mut stdin = io::stdin().lock();
    let code = loop {
        match read_frame(&mut stdin) {
            Ok(Some(body)) => {
                if let Err(error) = host.handle_extension_message(&body) {
                    eprintln!("browvitab host: {error}");
                }
            }
            Ok(None) => break 0,
            Err(error) => {
                eprintln!("browvitab host: {error}");
                break 1;
            }
        }
    };
    let _ = fs::remove_file(&socket);
    code
}

fn timed_out(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message.to_string())
}

fn time_left(deadline: Instant, message: &str) -> io::Result<Duration> {
    match deadline.checked_duration_since(Instant::now()) {
        Some(left) if !left.is_zero() => Ok(left),
        _ => Err(timed_out(message)),
    }
}

/// The host pid encoded in a socket name (`<pid>.sock`).
pub fn socket_pid(socket: &Path) -> Option<u32> {
    socket.file_stem()?.to_str()?.parse().ok()
}

/// Connect, giving up at `deadline`. macOS refuses connections at once
/// when a listener's backlog is full, so a refusal from a live host is
/// retried until the deadline.
fn connect_before(socket: &Path, deadline: Instant) -> io::Result<UnixStream> {
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionRefused
                    && socket_pid(socket).is_some_and(macos::alive) =>
            {
                time_left(deadline, "the browser host is not accepting connections")?;
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Send one request and read the answer, all before `deadline`. The host
/// drops requests that reach it after the deadline, so a request that timed
/// out here is not carried out later.
pub fn request_before(socket: &Path, request: &Request, deadline: Instant) -> io::Result<Response> {
    let mut stream = connect_before(socket, deadline)?;
    let mut message = serde_json::to_value(request)?;
    let left = time_left(deadline, "the browser host did not answer in time")?;
    message["expires"] = json!(monotonic_millis()? + left.as_millis() as u64);
    let mut body = serde_json::to_vec(&message)?;
    body.push(b'\n');
    stream.set_write_timeout(Some(left))?;
    stream.write_all(&body)?;

    let unanswered =
        "the browser host did not answer in time; the request may still be carried out";
    let mut answer = Vec::new();
    let mut chunk = [0u8; 16384];
    while !answer.ends_with(b"\n") {
        stream.set_read_timeout(Some(time_left(deadline, unanswered)?))?;
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(timed_out(unanswered))
            }
            Err(error) => return Err(error),
        };
        if read == 0 {
            break;
        }
        answer.extend_from_slice(&chunk[..read]);
        if answer.len() > MAX_FRAME as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "answer too large",
            ));
        }
    }
    serde_json::from_slice(&answer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Ask the host behind `socket` to focus or close a tab.
pub fn send_request(socket: &Path, req: &Request) -> Result<(), String> {
    let response = request_before(socket, req, Instant::now() + CLIENT_TIMEOUT).map_err(
        |error| match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                "the browser is no longer connected".to_string()
            }
            _ => format!("browser host: {error}"),
        },
    )?;
    match response.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// A connected browser: its socket, listing and change generation.
#[derive(Debug, Clone)]
pub struct Connected {
    pub socket: PathBuf,
    pub listing: Listing,
    pub generation: u64,
}

/// The host sockets in `dir` owned by this user.
pub fn host_sockets(dir: &Path) -> Vec<PathBuf> {
    let uid = current_uid();
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sockets: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "sock")
                && fs::symlink_metadata(path)
                    .is_ok_and(|meta| meta.file_type().is_socket() && meta.uid() == uid)
        })
        .collect();
    sockets.sort();
    sockets
}

/// Ask every host at once and wait at most `timeout` overall, so one stuck
/// host does not hide the others. Sockets whose host is gone are removed.
pub fn list_browsers(dir: &Path, timeout: Duration) -> Vec<Connected> {
    ask_all(dir, timeout, |_| Request::List)
}

/// Send `request(socket)` to every host in parallel.
pub fn ask_all(
    dir: &Path,
    timeout: Duration,
    request: impl Fn(&Path) -> Request + Sync,
) -> Vec<Connected> {
    let sockets = host_sockets(dir);
    let deadline = Instant::now() + timeout;
    let answers: Vec<io::Result<Response>> = thread::scope(|scope| {
        let handles: Vec<_> = sockets
            .iter()
            .map(|socket| {
                let request = request(socket);
                scope.spawn(move || request_before(socket, &request, deadline))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err(io::Error::other("request thread panicked")))
            })
            .collect()
    });
    sockets
        .into_iter()
        .zip(answers)
        .filter_map(|(socket, answer)| match answer {
            Ok(Response {
                listing: Some(listing),
                generation,
                ..
            }) => Some(Connected {
                socket,
                listing,
                generation: generation.unwrap_or(0),
            }),
            Ok(_) => None,
            Err(error) => {
                let stale = matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) && !socket_pid(&socket).is_some_and(macos::alive);
                if stale {
                    let _ = fs::remove_file(&socket);
                }
                if std::env::var_os("BROWVITAB_DEBUG").is_some() {
                    eprintln!("browvitab: {}: {error}", socket.display());
                }
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static SEQ: AtomicU32 = AtomicU32::new(0);

    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from(format!(
            "/tmp/rt-{}-{}-{}",
            name,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn frame(value: Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    fn sent(out: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
        let bytes = out.lock().unwrap().clone();
        let mut reader = &bytes[..];
        let mut messages = Vec::new();
        while let Some(body) = read_frame(&mut reader).unwrap() {
            messages.push(serde_json::from_slice(&body).unwrap());
        }
        messages
    }

    #[derive(Clone)]
    struct SharedOut(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedOut {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn host_with_tabs(tabs: Value) -> (Arc<Host<SharedOut>>, Arc<Mutex<Vec<u8>>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(Host::new(
            Browser {
                id: "firefox".into(),
                name: "Firefox".into(),
                pid: Some(42),
                ..Browser::default()
            },
            SharedOut(Arc::clone(&out)),
        ));
        host.handle_extension_message(&frame(
            json!({ "method": "browser/tabs-changed", "data": tabs }),
        ))
        .unwrap();
        (host, out)
    }

    #[test]
    fn native_host_invocation_detects_both_browsers() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_native_host_invocation(&args(&[
            "x",
            "chrome-extension://kcmipingpfbohfjckomimmahknoddnke/"
        ])));
        assert!(is_native_host_invocation(&args(&[
            "x",
            "/path/com.vicinae.vicinae.json",
            "firefox@vicinae.com"
        ])));
        assert!(!is_native_host_invocation(&args(&["x", "list"])));
    }

    #[test]
    fn frames_round_trip_and_reject_bad_sizes() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"a\":1}").unwrap();
        assert_eq!(read_frame(&mut &buf[..]).unwrap().unwrap(), b"{\"a\":1}");
        assert!(read_frame(&mut &[0u8, 0, 0, 0][..]).is_err());
        assert!(read_frame(&mut &[][..]).unwrap().is_none());
    }

    #[test]
    fn init_gets_a_response_with_the_same_id() {
        let (host, out) = host_with_tabs(json!([]));
        host.handle_extension_message(&frame(
            json!({ "id": 7, "method": "browser/init", "data": { "name": "Firefox" } }),
        ))
        .unwrap();
        assert_eq!(sent(&out), vec![json!({ "id": 7, "result": {} })]);
    }

    #[test]
    fn tabs_changed_replaces_the_listing_and_bumps_the_generation() {
        let (host, _) = host_with_tabs(json!([{ "id": 1, "title": "a", "url": "https://a" }]));
        let before = host.generation();
        host.handle_extension_message(&frame(json!({
            "method": "browser/tabs-changed",
            "data": [{ "id": 2, "title": "b", "url": "https://b", "pinned": true }]
        })))
        .unwrap();
        let response = host.handle_request(Request::List);
        let listing = response.listing.unwrap();
        assert_eq!(listing.tabs.len(), 1);
        assert!(listing.tabs[0].pinned);
        assert_eq!(response.generation, Some(before + 1));
    }

    #[test]
    fn focus_and_close_are_forwarded_to_the_extension() {
        let (host, out) = host_with_tabs(json!([{ "id": 5, "url": "https://a" }]));
        assert_eq!(
            host.handle_request(Request::Focus { tab_id: 5 }).error,
            None
        );
        assert_eq!(
            host.handle_request(Request::Close { tab_id: 5 }).error,
            None
        );
        assert_eq!(
            sent(&out),
            vec![
                json!({ "id": "browvitab-1", "method": "browser/focus-tab", "data": { "tabId": 5 } }),
                json!({ "id": "browvitab-2", "method": "browser/close-tab", "data": { "tabId": 5 } }),
            ]
        );
        // Closed tabs vanish at once.
        assert!(host
            .handle_request(Request::List)
            .listing
            .unwrap()
            .tabs
            .is_empty());
        assert!(host
            .handle_request(Request::Focus { tab_id: 5 })
            .error
            .is_some());
    }

    #[test]
    fn watch_returns_on_change_or_timeout() {
        let (host, _) = host_with_tabs(json!([{ "id": 1, "url": "https://a" }]));
        let since = host.generation();
        let start = Instant::now();
        let response = host.handle_request(Request::Watch {
            since,
            timeout_ms: 50,
        });
        assert!(start.elapsed() >= Duration::from_millis(45));
        assert_eq!(response.generation, Some(since));

        let waker = Arc::clone(&host);
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            waker
                .handle_extension_message(&frame(json!({
                    "method": "browser/tabs-changed", "data": []
                })))
                .unwrap();
        });
        let start = Instant::now();
        let response = host.handle_request(Request::Watch {
            since,
            timeout_ms: 5000,
        });
        t.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(response.generation, Some(since + 1));
        assert!(response.listing.unwrap().tabs.is_empty());

        // A stale generation answers immediately.
        let start = Instant::now();
        host.handle_request(Request::Watch {
            since: 0,
            timeout_ms: 5000,
        });
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn socket_clients_list_focus_and_watch() {
        let dir = temp_dir("sock");
        let (host, out) = host_with_tabs(json!([{ "id": 9, "title": "t", "url": "https://t" }]));
        // Name the socket after this (live) process.
        let socket = dir.join(format!("{}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = Arc::clone(&host);
        thread::spawn(move || serve_clients(listener, server, 4));

        let browsers = list_browsers(&dir, Duration::from_secs(2));
        assert_eq!(browsers.len(), 1);
        assert_eq!(browsers[0].listing.tabs[0].id, 9);
        send_request(&socket, &Request::Focus { tab_id: 9 }).unwrap();
        assert_eq!(sent(&out).len(), 1);
        assert!(send_request(&socket, &Request::Focus { tab_id: 99 }).is_err());

        let since = browsers[0].generation;
        let waker = Arc::clone(&host);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            waker.update_browser(|b| b.profile = Some("work".into()));
        });
        let watched = ask_all(&dir, Duration::from_secs(3), |_| Request::Watch {
            since,
            timeout_ms: 2000,
        });
        assert_eq!(watched[0].listing.browser.profile.as_deref(), Some("work"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn abandoned_watches_free_their_client_slot() {
        let dir = temp_dir("abandon");
        let (host, _) = host_with_tabs(json!([{ "id": 1, "url": "https://a" }]));
        let socket = dir.join(format!("{}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = Arc::clone(&host);
        // Room for exactly two clients.
        thread::spawn(move || serve_clients(listener, server, 2));
        let since = list_browsers(&dir, Duration::from_secs(1))[0].generation;
        for _ in 0..4 {
            let mut stream = UnixStream::connect(&socket).unwrap();
            let line = format!("{{\"method\":\"watch\",\"since\":{since},\"timeoutMs\":30000}}\n");
            stream.write_all(line.as_bytes()).unwrap();
            thread::sleep(Duration::from_millis(30));
            drop(stream);
        }
        thread::sleep(WATCH_SLICE + Duration::from_millis(150));
        let start = Instant::now();
        assert_eq!(list_browsers(&dir, Duration::from_secs(1)).len(), 1);
        assert!(start.elapsed() < Duration::from_millis(500));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_sockets_are_removed() {
        let dir = temp_dir("stale");
        // A pid that cannot exist.
        let socket = dir.join("999999999.sock");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        assert!(list_browsers(&dir, Duration::from_millis(300)).is_empty());
        assert!(!socket.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn expired_requests_are_not_carried_out() {
        let (host, out) = host_with_tabs(json!([{ "id": 1, "url": "https://a" }]));
        let response =
            host.handle_request_until(Request::Focus { tab_id: 1 }, Some(Instant::now()));
        assert!(response.error.is_some());
        assert!(sent(&out).is_empty());
    }

    #[test]
    fn browsers_are_classified_from_macos_bundles() {
        let c = |p: &str| classify_browser(Path::new(p)).map(|(id, _)| id);
        assert_eq!(
            c("/Applications/Firefox.app/Contents/MacOS/firefox"),
            Some("firefox")
        );
        assert_eq!(
            c("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
            Some("brave-browser")
        );
        assert_eq!(
            c("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            Some("google-chrome")
        );
        assert_eq!(
            c("/Applications/LibreWolf.app/Contents/MacOS/librewolf"),
            Some("librewolf")
        );
        assert_eq!(c("/bin/zsh"), None);
    }
}
