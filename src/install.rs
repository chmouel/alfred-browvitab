//! Native messaging manifest registration on macOS. Manifests that start Vicinae
//! or anything else not installed by us are never replaced without
//! `--force`.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::host::{CHROME_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME};

/// Written into every manifest we install, to tell them apart from
/// manifests of other hosts.
pub const MANIFEST_DESCRIPTION: &str =
    "Browvitab native messaging host for the Vicinae browser extension";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestKind {
    Chromium,
    Gecko,
}

/// Where a browser looks for native messaging manifests, if it is installed.
#[derive(Debug, Clone)]
pub struct ManifestTarget {
    pub browser: &'static str,
    /// The browser's own directory; nothing is written without it.
    pub probe: PathBuf,
    pub dir: PathBuf,
    pub kind: ManifestKind,
}

/// Per-user manifest locations of the browsers the Vicinae extension
/// supports, relative to `~/Library/Application Support`.
pub fn manifest_targets(support: &Path) -> Vec<ManifestTarget> {
    const CHROMIUM: [(&str, &str); 11] = [
        ("Google Chrome", "Google/Chrome"),
        ("Google Chrome Beta", "Google/Chrome Beta"),
        ("Google Chrome Dev", "Google/Chrome Dev"),
        ("Google Chrome Canary", "Google/Chrome Canary"),
        ("Chromium", "Chromium"),
        ("Brave", "BraveSoftware/Brave-Browser"),
        ("Microsoft Edge", "Microsoft Edge"),
        ("Vivaldi", "Vivaldi"),
        ("Arc", "Arc/User Data"),
        ("Thorium", "Thorium"),
        ("Helium", "net.imput.helium"),
    ];
    let mut targets: Vec<ManifestTarget> = CHROMIUM
        .iter()
        .map(|(browser, dir)| ManifestTarget {
            browser,
            probe: support.join(dir),
            dir: support.join(dir).join("NativeMessagingHosts"),
            kind: ManifestKind::Chromium,
        })
        .collect();
    // Firefox and most forks read the shared Mozilla directory.
    let mozilla = support.join("Mozilla").join("NativeMessagingHosts");
    let gecko: [(&str, &str, PathBuf); 6] = [
        ("Firefox", "Firefox", mozilla.clone()),
        ("Zen", "zen", mozilla.clone()),
        ("Floorp", "Floorp", mozilla.clone()),
        ("Waterfox", "Waterfox", mozilla.clone()),
        ("LibreWolf", "librewolf", mozilla.clone()),
        (
            "LibreWolf",
            "librewolf",
            support.join("librewolf").join("NativeMessagingHosts"),
        ),
    ];
    targets.extend(
        gecko
            .into_iter()
            .map(|(browser, probe, dir)| ManifestTarget {
                browser,
                probe: support.join(probe),
                dir,
                kind: ManifestKind::Gecko,
            }),
    );
    targets
}

pub fn manifest_json(kind: ManifestKind, exe: &Path) -> String {
    let mut manifest = json!({
        "name": HOST_NAME,
        "description": MANIFEST_DESCRIPTION,
        "path": exe.to_string_lossy(),
        "type": "stdio",
    });
    match kind {
        ManifestKind::Chromium => {
            manifest["allowed_origins"] =
                json!([format!("chrome-extension://{CHROME_EXTENSION_ID}/")]);
        }
        ManifestKind::Gecko => manifest["allowed_extensions"] = json!([FIREFOX_EXTENSION_ID]),
    }
    let mut text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    text.push('\n');
    text
}

/// Who an existing manifest belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// Installed by us, starting this program.
    Ours(PathBuf),
    Vicinae,
    /// Anything else, described for the user.
    Other(String),
}

/// The owner of the manifest at `path`, or None when there is none.
pub fn manifest_owner(path: &Path) -> Option<Owner> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return Some(Owner::Other("it is a symbolic link".to_string()));
    }
    if !meta.is_file() {
        return Some(Owner::Other("it is not a regular file".to_string()));
    }
    let manifest = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let Some(manifest) = manifest else {
        return Some(Owner::Other("it cannot be read as a manifest".to_string()));
    };
    let program = manifest
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let description = manifest.get("description").and_then(Value::as_str);
    Some(match program {
        Some(program) if description == Some(MANIFEST_DESCRIPTION) => Owner::Ours(program),
        Some(program) if is_vicinae(&program) => Owner::Vicinae,
        Some(program) => Owner::Other(format!("it starts {}", program.display())),
        None => Owner::Other("it names no program".to_string()),
    })
}

fn is_vicinae(program: &Path) -> bool {
    program
        .file_name()
        .is_some_and(|name| name.to_string_lossy().to_lowercase().contains("vicinae"))
}

/// Write through a temporary file and a rename, so the browser never reads
/// half a manifest. A symbolic link at `path` is replaced, not followed.
fn write_manifest(dir: &Path, path: &Path, content: &str, replace: Replace) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{HOST_NAME}.json.{}.tmp", std::process::id()));
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(content.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| match replace {
            Replace::Any => fs::rename(&temp, path),
            // A hard link fails if anything appeared at `path` meanwhile.
            Replace::Absent => fs::hard_link(&temp, path).map(|()| {
                let _ = fs::remove_file(&temp);
            }),
            // Only replace a manifest that is still ours.
            Replace::Ours => match manifest_owner(path) {
                Some(Owner::Ours(_)) => fs::rename(&temp, path),
                None => fs::hard_link(&temp, path).map(|()| {
                    let _ = fs::remove_file(&temp);
                }),
                Some(_) => Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "another program replaced the manifest meanwhile",
                )),
            },
        });
    let _ = fs::remove_file(&temp);
    result
}

#[derive(Clone, Copy)]
enum Replace {
    Any,
    Absent,
    Ours,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Written,
    Unchanged,
    /// Belongs to another host and was left alone; says why.
    Kept(String),
    Removed,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub browsers: Vec<&'static str>,
    pub path: PathBuf,
    pub outcome: Outcome,
}

fn group_targets(support: &Path) -> Vec<(Vec<&'static str>, ManifestTarget)> {
    let mut out: Vec<(Vec<&'static str>, ManifestTarget)> = Vec::new();
    for target in manifest_targets(support) {
        if !target.probe.is_dir() {
            continue;
        }
        if let Some((names, _)) = out.iter_mut().find(|(_, t)| t.dir == target.dir) {
            if !names.contains(&target.browser) {
                names.push(target.browser);
            }
            continue;
        }
        out.push((vec![target.browser], target));
    }
    out
}

/// Point the manifest of every installed browser at `exe`. A manifest we
/// did not install, or a symbolic link, is kept unless `force` is set.
pub fn install_manifests(support: &Path, exe: &Path, force: bool) -> Vec<Report> {
    group_targets(support)
        .into_iter()
        .map(|(browsers, target)| {
            let path = target.dir.join(format!("{HOST_NAME}.json"));
            let content = manifest_json(target.kind, exe);
            let is_link =
                fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink());
            let owner = manifest_owner(&path);
            let replace = match &owner {
                _ if force => Replace::Any,
                None => Replace::Absent,
                Some(_) => Replace::Ours,
            };
            let keep = match owner {
                _ if force => None,
                _ if is_link => Some("it is a symbolic link".to_string()),
                None | Some(Owner::Ours(_)) => None,
                Some(Owner::Vicinae) => Some("it starts Vicinae".to_string()),
                Some(Owner::Other(reason)) => Some(reason),
            };
            let outcome = if !is_link
                && fs::read_to_string(&path).is_ok_and(|existing| existing == content)
            {
                Outcome::Unchanged
            } else if let Some(reason) = keep {
                Outcome::Kept(reason)
            } else {
                match write_manifest(&target.dir, &path, &content, replace) {
                    Ok(()) => Outcome::Written,
                    Err(error) => Outcome::Failed(error.to_string()),
                }
            };
            Report {
                browsers,
                path,
                outcome,
            }
        })
        .collect()
}

/// Remove the manifests we installed.
pub fn uninstall_manifests(support: &Path) -> Vec<Report> {
    group_targets(support)
        .into_iter()
        .filter_map(|(browsers, target)| {
            let path = target.dir.join(format!("{HOST_NAME}.json"));
            if !matches!(manifest_owner(&path), Some(Owner::Ours(_))) {
                return None;
            }
            let outcome = match fs::remove_file(&path) {
                Ok(()) => Outcome::Removed,
                Err(error) => Outcome::Failed(error.to_string()),
            };
            Some(Report {
                browsers,
                path,
                outcome,
            })
        })
        .collect()
}

/// Every manifest present for installed browsers, with its owner.
pub fn status(support: &Path) -> Vec<(Vec<&'static str>, PathBuf, Option<Owner>)> {
    group_targets(support)
        .into_iter()
        .map(|(browsers, target)| {
            let path = target.dir.join(format!("{HOST_NAME}.json"));
            let owner = manifest_owner(&path);
            (browsers, path, owner)
        })
        .collect()
}

pub fn support_dir() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support"))
}

/// Stable home of the host binary, outside the (possibly synced) workflow
/// folder, so manifests keep working when Alfred preferences move.
pub fn install_dir(support: &Path) -> PathBuf {
    support.join("browvitab")
}

/// Copy `exe` into the install dir (atomically) and return the new path.
pub fn install_binary(support: &Path, exe: &Path) -> io::Result<PathBuf> {
    let dir = install_dir(support).join("bin");
    fs::create_dir_all(&dir)?;
    let dest = dir.join("browvitab");
    if fs::canonicalize(exe).ok() == fs::canonicalize(&dest).ok() {
        return Ok(dest);
    }
    let temp = dir.join(format!(".browvitab.{}.tmp", std::process::id()));
    fs::copy(exe, &temp)?;
    let result = fs::set_permissions(&temp, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .and_then(|()| fs::rename(&temp, &dest));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "browvitab-install-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Firefox")).unwrap();
        fs::create_dir_all(dir.join("BraveSoftware/Brave-Browser")).unwrap();
        dir
    }

    #[test]
    fn installs_only_for_present_browsers_and_is_idempotent() {
        let s = support();
        let exe = Path::new("/opt/browvitab");
        let reports = install_manifests(&s, exe, false);
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|r| r.outcome == Outcome::Written));
        let firefox = s.join("Mozilla/NativeMessagingHosts/com.vicinae.vicinae.json");
        let text = fs::read_to_string(&firefox).unwrap();
        assert!(text.contains("firefox@vicinae.com"));
        let again = install_manifests(&s, exe, false);
        assert!(again.iter().all(|r| r.outcome == Outcome::Unchanged));
        let removed = uninstall_manifests(&s);
        assert!(removed.iter().all(|r| r.outcome == Outcome::Removed));
        assert!(!firefox.exists());
        let _ = fs::remove_dir_all(s);
    }

    #[test]
    fn vicinae_and_foreign_manifests_are_kept_unless_forced() {
        let s = support();
        let dir = s.join("Mozilla/NativeMessagingHosts");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("com.vicinae.vicinae.json");
        fs::write(
            &path,
            r#"{"name":"com.vicinae.vicinae","path":"/usr/bin/vicinae"}"#,
        )
        .unwrap();
        let reports = install_manifests(&s, Path::new("/x/browvitab"), false);
        let ff = reports.iter().find(|r| r.path == path).unwrap();
        assert_eq!(ff.outcome, Outcome::Kept("it starts Vicinae".into()));
        assert!(fs::read_to_string(&path).unwrap().contains("vicinae\""));
        // Uninstall never removes it either.
        uninstall_manifests(&s);
        assert!(path.exists());
        let forced = install_manifests(&s, Path::new("/x/browvitab"), true);
        assert!(forced
            .iter()
            .any(|r| r.path == path && r.outcome == Outcome::Written));
        let _ = fs::remove_dir_all(s);
    }

    #[test]
    fn symlinked_manifests_survive_install_and_uninstall() {
        let s = support();
        let dir = s.join("Mozilla/NativeMessagingHosts");
        fs::create_dir_all(&dir).unwrap();
        let target = s.join("real.json");
        fs::write(&target, manifest_json(ManifestKind::Gecko, Path::new("/x"))).unwrap();
        let link = dir.join("com.vicinae.vicinae.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let reports = install_manifests(&s, Path::new("/y"), false);
        assert!(reports
            .iter()
            .any(|r| r.path == link && matches!(r.outcome, Outcome::Kept(_))));
        uninstall_manifests(&s);
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
        let _ = fs::remove_dir_all(s);
    }
}
