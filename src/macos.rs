//! macOS process inspection and application activation, without
//! Objective-C bindings: libproc for process facts and the Objective-C
//! runtime for `NSRunningApplication`.

use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Executable path of `pid`.
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buf is valid for the given size.
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr().cast::<c_void>(),
            buf.len() as u32,
        )
    };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    String::from_utf8(buf).ok().map(PathBuf::from)
}

/// Parent of `pid`.
pub fn parent_of(pid: u32) -> Option<u32> {
    // SAFETY: proc_bsdinfo is plain data; zeroes are valid.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: info is valid for `size` bytes.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast::<c_void>(),
            size,
        )
    };
    (got == size).then_some(info.pbi_ppid)
}

/// Whether a process with this pid exists.
pub fn alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: signal 0 only checks for existence.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The `.app` bundle containing `exe`, the outermost one so helpers inside
/// a browser bundle resolve to the browser.
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .last()
        .map(Path::to_path_buf)
}

/// The Firefox-family profile directory `pid` has open, found through the
/// `.parentlock` file Gecko keeps open for the profile it runs.
pub fn gecko_profile_dir(pid: u32) -> Option<PathBuf> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-Fn"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| line.strip_prefix('n'))
        .find_map(|name| name.strip_suffix("/.parentlock"))
        .map(PathBuf::from)
}

/// Profile name for a Gecko profile directory, read from the
/// `profiles.ini` next to it, or the directory name itself.
pub fn gecko_profile_name(dir: &Path) -> String {
    let fallback = || {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // "abcd1234.default-release" -> "default-release"
        name.split_once('.')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or(name)
    };
    for root in dir.ancestors().skip(1).take(3) {
        let Ok(ini) = std::fs::read_to_string(root.join("profiles.ini")) else {
            continue;
        };
        let mut name: Option<String> = None;
        let mut path: Option<String> = None;
        let mut relative = true;
        let check = |name: &Option<String>, path: &Option<String>, relative: bool| {
            let (Some(name), Some(path)) = (name, path) else {
                return None;
            };
            let full = if relative {
                root.join(path)
            } else {
                PathBuf::from(path)
            };
            (full == dir).then(|| name.clone())
        };
        for line in ini.lines().map(str::trim) {
            if line.starts_with('[') {
                if let Some(found) = check(&name, &path, relative) {
                    return found;
                }
                name = None;
                path = None;
                relative = true;
            } else if let Some(v) = line.strip_prefix("Name=") {
                name = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("Path=") {
                path = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("IsRelative=") {
                relative = v != "0";
            }
        }
        if let Some(found) = check(&name, &path, relative) {
            return found;
        }
    }
    fallback()
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

type Id = *mut c_void;

fn sel(name: &CStr) -> Id {
    // SAFETY: a NUL-terminated selector name.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// Bring the application owning `pid` to the front. Works per process, so
/// two instances of the same browser (two Firefox profiles) stay apart.
pub fn activate_pid(pid: u32) -> bool {
    const ACTIVATE_ALL_WINDOWS: usize = 1;
    const ACTIVATE_IGNORING_OTHER_APPS: usize = 2;
    // SAFETY: objc_msgSend is called through prototypes matching the
    // documented signatures of the selectors used.
    unsafe {
        let class = objc_getClass(c"NSRunningApplication".as_ptr());
        if class.is_null() {
            return false;
        }
        let with_pid: extern "C" fn(Id, Id, libc::pid_t) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        let app = with_pid(
            class,
            sel(c"runningApplicationWithProcessIdentifier:"),
            pid as libc::pid_t,
        );
        if app.is_null() {
            return false;
        }
        let activate: extern "C" fn(Id, Id, usize) -> i8 =
            std::mem::transmute(objc_msgSend as *const ());
        activate(
            app,
            sel(c"activateWithOptions:"),
            ACTIVATE_ALL_WINDOWS | ACTIVATE_IGNORING_OTHER_APPS,
        ) != 0
    }
}

/// Pid of the frontmost application.
pub fn frontmost_pid() -> Option<u32> {
    // SAFETY: as in activate_pid.
    unsafe {
        let class = objc_getClass(c"NSWorkspace".as_ptr());
        if class.is_null() {
            return None;
        }
        let send: extern "C" fn(Id, Id) -> Id = std::mem::transmute(objc_msgSend as *const ());
        let workspace = send(class, sel(c"sharedWorkspace"));
        if workspace.is_null() {
            return None;
        }
        let app = send(workspace, sel(c"frontmostApplication"));
        if app.is_null() {
            return None;
        }
        let pid: extern "C" fn(Id, Id) -> libc::pid_t =
            std::mem::transmute(objc_msgSend as *const ());
        Some(pid(app, sel(c"processIdentifier")) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_bundle_is_the_outermost() {
        let exe = Path::new(
            "/Applications/Firefox.app/Contents/MacOS/plugin-container.app/Contents/MacOS/x",
        );
        assert_eq!(
            app_bundle(exe),
            Some(PathBuf::from("/Applications/Firefox.app"))
        );
        assert_eq!(app_bundle(Path::new("/usr/bin/true")), None);
    }

    #[test]
    fn this_process_is_alive_and_has_a_parent() {
        let me = std::process::id();
        assert!(alive(me));
        assert!(parent_of(me).is_some());
        assert!(exe_path(me).is_some());
    }

    #[test]
    fn profile_names_come_from_profiles_ini() {
        let root = std::env::temp_dir().join(format!("browvitab-ini-{}", std::process::id()));
        let dir = root.join("Profiles").join("abcd.work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            root.join("profiles.ini"),
            "[Profile0]\nName=Work\nIsRelative=1\nPath=Profiles/abcd.work\n\n[General]\nVersion=2\n",
        )
        .unwrap();
        assert_eq!(gecko_profile_name(&dir), "Work");
        assert_eq!(
            gecko_profile_name(&root.join("Profiles").join("zzzz.default-release")),
            "default-release"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
