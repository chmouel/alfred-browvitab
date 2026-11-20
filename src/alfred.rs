//! Output for Alfred: dataview data and JSON Actions for the Alfred UI,
//! and Script Filter JSON for the classic `tabs` result list. Both come
//! from the same ordered `TabView`s.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::tabs::{display_host, match_text, TabView};

/// Alfred UI expands `{var:…}`, `{file:…}` and friends in JSON it is given,
/// including data from scripts. Page titles are untrusted, so break up
/// anything that looks like a placeholder with a zero-width space.
pub fn defuse(text: &str) -> String {
    const PREFIXES: [&str; 7] = [
        "var:", "const:", "file:", "fileraw:", "value:", "data:", "param:",
    ];
    if !text.contains('{') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(pos) = rest.find('{') {
        out.push_str(&rest[..=pos]);
        rest = &rest[pos + 1..];
        let lower = rest.to_ascii_lowercase();
        if PREFIXES.iter().any(|p| lower.starts_with(p)) {
            out.push('\u{200B}');
        }
    }
    out.push_str(rest);
    out
}

fn subtitle(tab: &TabView) -> String {
    let host = display_host(&tab.host);
    let mut parts: Vec<String> = Vec::new();
    if !host.is_empty() {
        parts.push(host.to_string());
    } else if !tab.url.is_empty() {
        parts.push(
            tab.url
                .split(['?', '#'])
                .next()
                .unwrap_or(&tab.url)
                .to_string(),
        );
    }
    if !tab.source.is_empty() {
        parts.push(tab.source.clone());
    }
    if tab.muted {
        parts.push("Muted".into());
    } else if tab.audible {
        parts.push("Playing".into());
    }
    parts.join("  ·  ")
}

fn app_icon(tab: &TabView) -> Value {
    match &tab.app {
        Some(app) => json!({ "type": "fileicon", "src": app }),
        None => json!({ "type": "symbol", "src": "globe" }),
    }
}

/// One dataview entry. Keys are `componentid.property` of the cell in
/// `tabs.alfredui/components/tablist.json`; plain keys are data for
/// actions (`{data:taburl}`, `{value:tabs.selected.taburl}`) and matching.
pub fn ui_entry(tab: &TabView, icons: &HashMap<String, PathBuf>) -> Value {
    let favicon = icons
        .get(&tab.host)
        .map(|p| json!({ "type": "file", "src": p.to_string_lossy() }))
        .unwrap_or_else(|| app_icon(tab));
    let mut entry = Map::new();
    entry.insert("uid".into(), json!(tab.uid));
    entry.insert("title.text".into(), json!(defuse(&tab.title)));
    entry.insert("subtitle.text".into(), json!(defuse(&subtitle(tab))));
    entry.insert("favicon.image".into(), favicon);
    entry.insert("browsericon.image".into(), app_icon(tab));
    entry.insert("browsericon.tooltip".into(), json!(defuse(&tab.source)));
    if tab.pinned {
        entry.insert("pin.hidden".into(), json!(false));
    }
    if tab.muted || tab.audible {
        entry.insert("sound.hidden".into(), json!(false));
        entry.insert(
            "sound.image".into(),
            json!({ "type": "symbol", "src": if tab.muted { "speaker.slash.fill" } else { "speaker.wave.2.fill" } }),
        );
    }
    entry.insert("match".into(), json!(defuse(&match_text(tab))));
    entry.insert("taburl".into(), json!(defuse(&tab.url)));
    entry.insert("tabtitle".into(), json!(defuse(&tab.title)));
    entry.insert(
        "tabmd".into(),
        json!(defuse(&format!(
            "[{}]({})",
            tab.title.replace(['[', ']'], ""),
            tab.url
        ))),
    );
    entry.insert("tabgroup".into(), json!(defuse(&tab.group)));
    Value::Object(entry)
}

/// The filter, repeated on every data update so replacing the data never
/// drops it. `data.filter` is not preprocessed, so the placeholder stays
/// live and is read when the dataview filters.
pub fn filter() -> Value {
    json!({
        "type": "basic",
        "args": "match",
        "match": "{value:search.text}",
        "matchmode": "wordmatchanyorder"
    })
}

/// FNV-1a over the browser state, to tell when a refresh has news.
pub fn fingerprint(data: &[Value]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in serde_json::to_vec(data).unwrap_or_default() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub struct UiState {
    pub data: Vec<Value>,
    pub fingerprint: String,
    pub tab_count: usize,
    pub browsers: Vec<String>,
    pub setup_needed: bool,
}

pub fn status_text(state: &UiState) -> String {
    if state.browsers.is_empty() {
        return "No browser connected".into();
    }
    let tabs = match state.tab_count {
        1 => "1 tab".to_string(),
        n => format!("{n} tabs"),
    };
    format!("{tabs}  ·  {}", state.browsers.join(", "))
}

pub enum Select {
    /// Keep the row the user has selected, read when the actions run.
    Current,
    /// Reset to the first visible row.
    First,
    Uid(String),
}

/// JSON Actions replacing the dataview contents and status.
pub fn ui_update(state: &UiState, select: Select, placeholder: &str) -> Value {
    let mut variables = Map::new();
    variables.insert("fingerprint".into(), json!(state.fingerprint));
    let select = match select {
        Select::Current => {
            variables.insert("cursel".into(), json!("{value:tabs.selected.uid}"));
            json!("{var:cursel}")
        }
        Select::First => json!(""),
        Select::Uid(uid) => json!(uid),
    };
    json!({
        "variables": variables,
        "update": {
            "ids": {
                "tabs": {
                    "placeholder": placeholder,
                    "data": {
                        "type": "basic",
                        "src": state.data,
                        "filter": filter(),
                        "select": select
                    }
                },
                "status": { "text": status_text(state) },
                "setup": { "hidden": !state.setup_needed }
            }
        },
        "functions": { "ids": { "tabs": "filterdata" } }
    })
}

/// Script Filter JSON for the classic result list.
pub fn script_filter(
    tabs: &[TabView],
    icons: &HashMap<String, PathBuf>,
    setup_hint: Option<&str>,
) -> Value {
    let mut items: Vec<Value> = tabs
        .iter()
        .map(|tab| {
            let mut title = tab.title.clone();
            if tab.pinned {
                title = format!("📌 {title}");
            }
            let icon = match icons.get(&tab.host) {
                Some(path) => json!({ "path": path, "resize": "fit" }),
                None => match &tab.app {
                    Some(app) => json!({ "type": "fileicon", "path": app, "resize": "fit" }),
                    None => json!({ "path": "icon.png", "resize": "fit" }),
                },
            };
            json!({
                "uid": tab.uid,
                "title": title,
                "subtitle": subtitle(tab),
                "arg": tab.uid,
                "match": match_text(tab),
                "autocomplete": tab.title,
                "icon": icon,
                "quicklookurl": tab.url,
                "action": { "url": tab.url },
                "text": { "copy": tab.url, "largetype": tab.url },
                "variables": { "tabaction": "focus" },
                "mods": {
                    "cmd": {
                        "arg": tab.url,
                        "subtitle": format!("Copy URL: {}", tab.url),
                        "variables": { "tabaction": "copy" }
                    },
                    "ctrl": {
                        "arg": tab.uid,
                        "subtitle": format!("Close tab in {}", tab.source),
                        "variables": { "tabaction": "close" }
                    }
                }
            })
        })
        .collect();
    if items.is_empty() {
        items.push(json!({
            "title": "No browser tabs",
            "subtitle": setup_hint.unwrap_or("Is the Vicinae extension connected? Type “tabs:setup” to register the host."),
            "valid": false,
            "icon": { "path": "icon.png" }
        }));
    }
    json!({
        "skipknowledge": true,
        "strictmods": true,
        "iconresize": "fit",
        "items": items
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_in_titles_are_defused() {
        assert_eq!(defuse("plain {curly}"), "plain {curly}");
        assert_eq!(defuse("{file:/etc/passwd}"), "{\u{200B}file:/etc/passwd}");
        assert_eq!(defuse("a {VAR:x} b"), "a {\u{200B}VAR:x} b");
    }

    #[test]
    fn fingerprints_follow_content() {
        let a = vec![json!({"uid": "1:1"})];
        let b = vec![json!({"uid": "1:2"})];
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
}
