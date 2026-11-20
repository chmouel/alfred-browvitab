//! Site icons from the browsers' own favicon databases, without any
//! network access. Lookups run through the system
//! `sqlite3` with `immutable=1`, so locked databases are read without
//! waiting, and results are cached per host in
//! `~/Library/Caches/browvitab/favicons` (misses are retried weekly).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::host::is_gecko;
use crate::tabs::TabView;

const MISS_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const EXTENSIONS: [&str; 6] = ["png", "ico", "svg", "jpg", "gif", "webp"];
/// Bound the first-run cost; the rest are found on later openings.
const MAX_LOOKUPS: usize = 150;

pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("BROWVITAB_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("favicons"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Caches/browvitab/favicons"))
}

fn key(host: &str) -> String {
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

enum Cached {
    Icon(PathBuf),
    Miss,
    Unknown,
}

fn cached(dir: &Path, host: &str) -> Cached {
    let k = key(host);
    for ext in EXTENSIONS {
        let path = dir.join(format!("{k}.{ext}"));
        if path.is_file() {
            return Cached::Icon(path);
        }
    }
    let miss = dir.join(format!("{k}.none"));
    match fs::metadata(&miss).and_then(|m| m.modified()) {
        Ok(time) if SystemTime::now().duration_since(time).unwrap_or_default() < MISS_TTL => {
            Cached::Miss
        }
        _ => Cached::Unknown,
    }
}

fn support() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
}

fn chromium_root(browser_id: &str) -> Option<&'static str> {
    Some(match browser_id {
        "google-chrome" => "Google/Chrome",
        "google-chrome-beta" => "Google/Chrome Beta",
        "google-chrome-dev" => "Google/Chrome Dev",
        "google-chrome-canary" => "Google/Chrome Canary",
        "chromium" => "Chromium",
        "brave-browser" => "BraveSoftware/Brave-Browser",
        "microsoft-edge" => "Microsoft Edge",
        "vivaldi" => "Vivaldi",
        "arc" => "Arc/User Data",
        "thorium" => "Thorium",
        "helium" => "net.imput.helium",
        _ => return None,
    })
}

fn gecko_root(browser_id: &str) -> &'static str {
    match browser_id {
        "librewolf" => "librewolf",
        "zen" => "zen",
        "floorp" => "Floorp",
        "waterfox" => "Waterfox",
        _ => "Firefox",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Schema {
    Gecko,
    Chromium,
}

/// Favicon databases for a tab's browser, most specific first.
fn databases(tab: &TabView) -> Vec<(Schema, PathBuf)> {
    if is_gecko(&tab.browser_id) {
        if let Some(dir) = &tab.profile_dir {
            return vec![(Schema::Gecko, Path::new(dir).join("favicons.sqlite"))];
        }
        let Some(support) = support() else {
            return Vec::new();
        };
        let profiles = support.join(gecko_root(&tab.browser_id)).join("Profiles");
        return fs::read_dir(profiles)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| (Schema::Gecko, e.path().join("favicons.sqlite")))
            .filter(|(_, p)| p.is_file())
            .collect();
    }
    let (Some(support), Some(root)) = (support(), chromium_root(&tab.browser_id)) else {
        return Vec::new();
    };
    let root = support.join(root);
    let mut dbs: Vec<(Schema, PathBuf)> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name == "Default" || name.starts_with("Profile ")
        })
        .map(|e| (Schema::Chromium, e.path().join("Favicons")))
        .filter(|(_, p)| p.is_file())
        .collect();
    dbs.sort_by_key(|(_, p)| !p.to_string_lossy().contains("/Default/"));
    dbs
}

fn sql_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn uri(path: &Path) -> String {
    let mut out = String::from("file:");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'%' | b'?' | b'#' | b' ' | b'\'' | b'"' => out.push_str(&format!("%{b:02X}")),
            _ => out.push(b as char),
        }
    }
    out.push_str("?immutable=1");
    out
}

fn query(schema: Schema, host: &str, temp: &Path) -> String {
    let like_s = sql_quote(&format!("https://{host}/%"));
    let like = sql_quote(&format!("http://{host}/%"));
    let out = sql_quote(&temp.to_string_lossy());
    match schema {
        Schema::Gecko => format!(
            "SELECT writefile({out}, i.data) FROM moz_pages_w_icons p \
             JOIN moz_icons_to_pages ip ON ip.page_id = p.id \
             JOIN moz_icons i ON i.id = ip.icon_id \
             WHERE p.page_url LIKE {like_s} OR p.page_url LIKE {like} \
             ORDER BY abs(i.width - 32) LIMIT 1;\n"
        ),
        Schema::Chromium => format!(
            "SELECT writefile({out}, b.image_data) FROM icon_mapping m \
             JOIN favicon_bitmaps b ON b.icon_id = m.icon_id \
             WHERE m.page_url LIKE {like_s} OR m.page_url LIKE {like} \
             ORDER BY abs(b.width - 32) LIMIT 1;\n"
        ),
    }
}

fn sniff(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0, 0, 1, 0, ..] => "ico",
        [0xFF, 0xD8, ..] => "jpg",
        [b'G', b'I', b'F', ..] => "gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).to_lowercase();
            if head.contains("<svg") || head.starts_with("<?xml") {
                "svg"
            } else {
                "png"
            }
        }
    }
}

/// Run all lookups for one database in a single sqlite3 process.
fn lookup(schema: Schema, db: &Path, hosts: &[String], dir: &Path) {
    let temp = |host: &str| dir.join(format!("{}.{}.tmp", key(host), std::process::id()));
    let sql: String = hosts.iter().map(|h| query(schema, h, &temp(h))).collect();
    let child = Command::new("/usr/bin/sqlite3")
        .arg("-batch")
        .arg(uri(db))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(sql.as_bytes());
    }
    let _ = child.wait();
    for host in hosts {
        let t = temp(host);
        if let Ok(bytes) = fs::read(&t) {
            if !bytes.is_empty() {
                let dest = dir.join(format!("{}.{}", key(host), sniff(&bytes)));
                let _ = fs::rename(&t, dest);
            }
        }
        let _ = fs::remove_file(&t);
    }
}

/// Icon paths by host for `tabs`, looking up hosts not cached yet.
pub fn resolve(tabs: &[TabView]) -> HashMap<String, PathBuf> {
    let mut icons = HashMap::new();
    let Some(dir) = cache_dir() else {
        return icons;
    };
    if fs::create_dir_all(&dir).is_err() {
        return icons;
    }
    let mut wanted: HashMap<(Schema, PathBuf), Vec<String>> = HashMap::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut lookups = 0;
    for tab in tabs {
        if tab.host.is_empty() || !seen.insert(tab.host.as_str()) {
            continue;
        }
        match cached(&dir, &tab.host) {
            Cached::Icon(path) => {
                icons.insert(tab.host.clone(), path);
            }
            Cached::Miss => {}
            Cached::Unknown if lookups < MAX_LOOKUPS => {
                lookups += 1;
                for db in databases(tab) {
                    wanted.entry(db).or_default().push(tab.host.clone());
                }
            }
            Cached::Unknown => {}
        }
    }
    if wanted.is_empty() {
        return icons;
    }
    let mut pending: HashSet<String> = wanted.values().flatten().cloned().collect();
    let mut dbs: Vec<_> = wanted.into_iter().collect();
    dbs.sort_by(|a, b| a.0 .1.cmp(&b.0 .1));
    for ((schema, db), hosts) in dbs {
        let hosts: Vec<String> = hosts.into_iter().filter(|h| pending.contains(h)).collect();
        if hosts.is_empty() {
            continue;
        }
        lookup(schema, &db, &hosts, &dir);
        for host in hosts {
            if let Cached::Icon(path) = cached(&dir, &host) {
                pending.remove(&host);
                icons.insert(host, path);
            }
        }
    }
    for host in pending {
        let _ = fs::write(dir.join(format!("{}.none", key(&host))), b"");
    }
    icons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_uris_are_safe() {
        assert_eq!(key("a.b-c.com"), "a.b-c.com");
        assert_eq!(key("[::1]"), "___1_");
        assert_eq!(
            uri(Path::new(
                "/Users/x/Library/Application Support/F/favicons.sqlite"
            )),
            "file:/Users/x/Library/Application%20Support/F/favicons.sqlite?immutable=1"
        );
        assert_eq!(sql_quote("it's"), "'it''s'");
    }

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 0]), "png");
        assert_eq!(sniff(&[0, 0, 1, 0, 1]), "ico");
        assert_eq!(sniff(b"<svg xmlns=..."), "svg");
    }

    #[test]
    fn looks_up_a_gecko_database() {
        let dir = std::env::temp_dir().join(format!("browvitab-fav-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("favicons.sqlite");
        let sql = "CREATE TABLE moz_pages_w_icons(id INTEGER PRIMARY KEY, page_url TEXT);\
                   CREATE TABLE moz_icons(id INTEGER PRIMARY KEY, width INT, data BLOB);\
                   CREATE TABLE moz_icons_to_pages(page_id INT, icon_id INT);\
                   INSERT INTO moz_pages_w_icons VALUES(1,'https://example.org/a');\
                   INSERT INTO moz_icons VALUES(1,16,X'89504E470D0A');\
                   INSERT INTO moz_icons_to_pages VALUES(1,1);";
        let status = Command::new("/usr/bin/sqlite3")
            .arg(&db)
            .arg(sql)
            .status()
            .unwrap();
        assert!(status.success());
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        lookup(
            Schema::Gecko,
            &db,
            &["example.org".into(), "none.test".into()],
            &cache,
        );
        assert!(cache.join("example.org.png").is_file());
        assert!(matches!(cached(&cache, "none.test"), Cached::Unknown));
        let _ = fs::remove_dir_all(dir);
    }
}
