//! Tab ordering and ranking.
//!
//! Without a query: pinned tabs of every browser first, then one section
//! per browser instance, the one used last on top. Within each list tabs
//! playing sound come first, then the most recently used, and the tab the
//! user is looking at goes last, so the previous tab is the first row.
//!
//! With a query the Alfred UI filters natively; `rank` only reorders so the
//! best matches lead and the sections no longer apply.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::host::{socket_pid, Connected, Tab};

/// A tab ready to display, with everything the front ends need.
#[derive(Debug, Clone, PartialEq)]
pub struct TabView {
    /// `<host pid>:<tab id>`: stable for the life of the browser session
    /// and unique across browsers and profiles.
    pub uid: String,
    pub socket: PathBuf,
    pub tab_id: i64,
    pub title: String,
    pub url: String,
    pub host: String,
    pub browser_id: String,
    pub browser_name: String,
    /// Browser label with the profile when several instances of the same
    /// browser are connected, e.g. "Firefox · work".
    pub source: String,
    pub app: Option<String>,
    pub profile_dir: Option<String>,
    pub browser_pid: Option<u32>,
    pub pinned: bool,
    pub audible: bool,
    pub muted: bool,
    pub current: bool,
    pub last_accessed: f64,
    /// "Pinned" or the browser section label.
    pub group: String,
}

/// The host of `url` (no port, no `www.`), or empty for non-web URLs.
pub fn url_host(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return String::new();
    };
    if !matches!(scheme, "http" | "https") {
        return String::new();
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = if authority.starts_with('[') {
        authority
            .split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    host.to_lowercase()
}

pub fn display_host(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

type Row<'a> = (&'a Connected, &'a Tab);

/// Order the tabs of `browsers` as described in the module docs.
pub fn order(browsers: &[Connected]) -> Vec<TabView> {
    let mut pinned: Vec<Row> = Vec::new();
    let mut sections: Vec<Vec<Row>> = Vec::new();
    for connected in browsers {
        let (pins, rest): (Vec<Row>, Vec<Row>) = connected
            .listing
            .tabs
            .iter()
            .map(|tab| (connected, tab))
            .partition(|(_, tab)| tab.pinned);
        pinned.extend(pins);
        if !rest.is_empty() {
            sections.push(rest);
        }
    }
    let current = pinned
        .iter()
        .chain(sections.iter().flatten())
        .filter(|(_, tab)| tab.last_accessed > 0.0)
        .max_by(|(_, a), (_, b)| a.last_accessed.total_cmp(&b.last_accessed))
        .filter(|(_, tab)| tab.active)
        .map(|(c, tab)| (c.socket.as_path(), tab.id));
    let is_current = |(c, tab): &Row| current == Some((c.socket.as_path(), tab.id));
    let by_use = |a: &Row, b: &Row| {
        is_current(a)
            .cmp(&is_current(b))
            .then(b.1.audible.cmp(&a.1.audible))
            .then(b.1.last_accessed.total_cmp(&a.1.last_accessed))
    };
    pinned.sort_by(by_use);
    for section in &mut sections {
        section.sort_by(by_use);
    }
    let last_used = |section: &[Row]| {
        section
            .iter()
            .map(|(_, t)| t.last_accessed)
            .fold(f64::MIN, f64::max)
    };
    sections.sort_by(|a, b| last_used(b).total_cmp(&last_used(a)));

    let sources = source_labels(browsers);
    let view = |(c, tab): Row, group: String| -> TabView {
        let browser = &c.listing.browser;
        let host = url_host(&tab.url);
        let stem = socket_pid(&c.socket)
            .map(|p| p.to_string())
            .unwrap_or_else(|| {
                c.socket
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            });
        TabView {
            uid: format!("{stem}:{}", tab.id),
            socket: c.socket.clone(),
            tab_id: tab.id,
            title: if tab.title.trim().is_empty() {
                tab.url.clone()
            } else {
                tab.title.clone()
            },
            url: tab.url.clone(),
            host,
            browser_id: browser.id.clone(),
            browser_name: browser_label(&browser.name, &browser.id),
            source: sources.get(c.socket.as_path()).cloned().unwrap_or_default(),
            app: browser.app.clone(),
            profile_dir: browser.profile_dir.clone(),
            browser_pid: browser.pid,
            pinned: tab.pinned,
            audible: tab.audible,
            muted: tab.muted,
            current: current == Some((c.socket.as_path(), tab.id)),
            last_accessed: tab.last_accessed,
            group,
        }
    };
    let mut out: Vec<TabView> = pinned
        .into_iter()
        .map(|row| view(row, "Pinned".to_string()))
        .collect();
    for section in sections {
        let label = sources
            .get(section[0].0.socket.as_path())
            .cloned()
            .unwrap_or_default();
        out.extend(section.into_iter().map(|row| view(row, label.clone())));
    }
    out
}

fn browser_label(name: &str, id: &str) -> String {
    [name, id]
        .into_iter()
        .find(|n| !n.is_empty())
        .unwrap_or("Browser")
        .to_string()
}

/// "Firefox", or "Firefox · work" when several Firefox instances (profiles)
/// are connected at once.
fn source_labels(browsers: &[Connected]) -> HashMap<&Path, String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for c in browsers {
        *counts.entry(c.listing.browser.id.as_str()).or_default() += 1;
    }
    browsers
        .iter()
        .map(|c| {
            let b = &c.listing.browser;
            let name = browser_label(&b.name, &b.id);
            let label = if counts.get(b.id.as_str()).copied().unwrap_or(0) > 1 {
                match b.profile.as_deref().filter(|p| !p.is_empty()) {
                    Some(profile) => format!("{name} · {profile}"),
                    None => match b.pid {
                        Some(pid) => format!("{name} · {pid}"),
                        None => name,
                    },
                }
            } else {
                name
            };
            (c.socket.as_path(), label)
        })
        .collect()
}

/// Words of a URL, split on punctuation, so word-start matching reaches
/// path segments ("openshift-pipelines/pull" -> "openshift pipelines pull").
fn url_words(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the native filters match against: title, host, the URL words and
/// the full URL, so a pasted address matches too.
pub fn match_text(tab: &TabView) -> String {
    let mut text = format!("{} {}", tab.title, display_host(&tab.host));
    if !tab.host.is_empty() && tab.host != display_host(&tab.host) {
        text.push(' ');
        text.push_str(&tab.host);
    }
    text.push(' ');
    text.push_str(&url_words(&tab.url));
    text.push(' ');
    text.push_str(&tab.url);
    text
}

/// Approximates the dataview's "word match, any order" filter: every
/// query word starts a word of the match text.
pub fn matches(tab: &TabView, query: &str) -> bool {
    let text = match_text(tab).to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| word_starts(&text, word))
}

fn word_starts(haystack: &str, needle: &str) -> bool {
    let mut start = true;
    for (i, c) in haystack.char_indices() {
        if start && haystack[i..].starts_with(needle) {
            return true;
        }
        start = !c.is_alphanumeric();
    }
    false
}

fn word_score(tab: &TabView, title: &str, url: &str, word: &str) -> u32 {
    let host = display_host(&tab.host);
    if title.starts_with(word) {
        100
    } else if host.starts_with(word) {
        90
    } else if word_starts(title, word) {
        70
    } else if word_starts(host, word) {
        60
    } else if word_starts(url, word) {
        35
    } else if title.contains(word) || url.contains(word) {
        10
    } else {
        0
    }
}

/// Reorder `tabs` for `query`: best matches first, ties kept in recency order. A pasted URL puts the tabs of that site first.
pub fn rank(tabs: Vec<TabView>, query: &str) -> Vec<TabView> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return tabs;
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let pasted_host = url_host(&query);
    let pasted_host = if pasted_host.is_empty() && words.len() == 1 && query.contains('.') {
        url_host(&format!("https://{query}"))
    } else {
        pasted_host
    };
    let mut scored: Vec<(u32, usize, TabView)> = tabs
        .into_iter()
        .enumerate()
        .map(|(i, tab)| {
            let title = tab.title.to_lowercase();
            let url = tab.url.to_lowercase();
            let mut score: u32 = words
                .iter()
                .map(|w| word_score(&tab, &title, &url, w))
                .sum();
            if url == query {
                score += 1000;
            }
            if !pasted_host.is_empty() && display_host(&tab.host) == display_host(&pasted_host) {
                score += 500;
            }
            // The tab already in view is the least useful match.
            if tab.current {
                score = score.saturating_sub(5);
            }
            (score, i, tab)
        })
        .collect();
    scored.sort_by(|a, b| match b.0.cmp(&a.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    scored.into_iter().map(|(_, _, tab)| tab).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Browser, Listing};

    fn tab(id: i64, title: &str, url: &str, last: f64) -> Tab {
        Tab {
            id,
            title: title.into(),
            url: url.into(),
            last_accessed: last,
            ..Tab::default()
        }
    }

    fn connected(
        pid: u32,
        id: &str,
        name: &str,
        profile: Option<&str>,
        tabs: Vec<Tab>,
    ) -> Connected {
        Connected {
            socket: PathBuf::from(format!("/tmp/x/{pid}.sock")),
            listing: Listing {
                browser: Browser {
                    id: id.into(),
                    name: name.into(),
                    pid: Some(pid + 1000),
                    profile: profile.map(Into::into),
                    ..Browser::default()
                },
                tabs,
            },
            generation: 1,
        }
    }

    fn titles(tabs: &[TabView]) -> Vec<&str> {
        tabs.iter().map(|t| t.title.as_str()).collect()
    }

    #[test]
    fn pinned_first_then_sections_by_last_use_current_last() {
        let mut current = tab(1, "current", "https://c.test", 900.0);
        current.active = true;
        let mut playing = tab(2, "playing", "https://p.test", 100.0);
        playing.audible = true;
        let mut pin = tab(3, "pin", "https://pin.test", 50.0);
        pin.pinned = true;
        let firefox = connected(
            10,
            "firefox",
            "Firefox",
            None,
            vec![
                current,
                tab(4, "older", "https://o.test", 200.0),
                playing,
                pin,
            ],
        );
        let brave = connected(
            20,
            "brave-browser",
            "Brave",
            None,
            vec![tab(5, "brave", "https://b.test", 300.0)],
        );
        let ordered = order(&[brave, firefox]);
        assert_eq!(
            titles(&ordered),
            vec!["pin", "playing", "older", "current", "brave"],
            "firefox was used last so its section leads; the tab in view goes last"
        );
        assert_eq!(ordered[0].group, "Pinned");
        assert_eq!(ordered[1].group, "Firefox");
        assert!(ordered[3].current);
        assert_eq!(ordered[4].uid, "20:5");
    }

    #[test]
    fn profiles_label_instances_of_the_same_browser() {
        let a = connected(
            1,
            "firefox",
            "Firefox",
            Some("work"),
            vec![tab(1, "a", "https://a", 1.0)],
        );
        let b = connected(
            2,
            "firefox",
            "Firefox",
            Some("home"),
            vec![tab(1, "b", "https://b", 2.0)],
        );
        let ordered = order(&[a, b]);
        assert_eq!(ordered[0].source, "Firefox · home");
        assert_eq!(ordered[1].source, "Firefox · work");
        assert_ne!(
            ordered[0].uid, ordered[1].uid,
            "same tab id, different hosts"
        );
        let single = order(&[connected(
            1,
            "firefox",
            "Firefox",
            Some("work"),
            vec![tab(1, "a", "https://a", 1.0)],
        )]);
        assert_eq!(single[0].source, "Firefox");
    }

    #[test]
    fn hosts_and_match_text() {
        assert_eq!(url_host("https://www.GitHub.com:443/a?b"), "www.github.com");
        assert_eq!(url_host("about:config"), "");
        assert_eq!(url_host("moz-extension://x/y"), "");
        let ordered = order(&[connected(
            1,
            "firefox",
            "Firefox",
            None,
            vec![tab(
                1,
                "PR",
                "https://github.com/openshift-pipelines/pipelines-as-code/pull/1",
                1.0,
            )],
        )]);
        let m = match_text(&ordered[0]);
        assert!(m.contains(" pipelines as code pull 1 "));
        assert!(m.ends_with("https://github.com/openshift-pipelines/pipelines-as-code/pull/1"));
    }

    #[test]
    fn ranking_puts_title_matches_first_and_pasted_urls_by_site() {
        let ordered = order(&[connected(
            1,
            "firefox",
            "Firefox",
            None,
            vec![
                tab(1, "Tekton docs", "https://tekton.dev/docs", 5.0),
                tab(2, "Pull requests", "https://github.com/pulls", 4.0),
                tab(3, "Gmail", "https://mail.google.com/", 3.0),
                tab(4, "Something about github", "https://example.com/", 2.0),
            ],
        )]);
        assert_eq!(
            titles(&rank(ordered.clone(), "git"))[..2],
            ["Pull requests", "Something about github"]
        );
        assert_eq!(
            titles(&rank(ordered.clone(), "https://github.com/foo"))[0],
            "Pull requests"
        );
        assert_eq!(titles(&rank(ordered.clone(), "  ")), titles(&ordered));
        assert_eq!(
            titles(&rank(ordered.clone(), "mail.google.com"))[0],
            "Gmail"
        );
        assert!(matches(&ordered[1], "pull git"));
        assert!(!matches(&ordered[1], "ull"));
    }
}
