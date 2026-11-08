//! Command history: every run, pull and shell the app starts, with the
//! equivalent docker command, so it can be repeated, edited or copied.
//! Stored in the app config directory as `history.json` (newest first).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::engine::RunRequest;
use crate::error::Result;

const MAX_ENTRIES: usize = 500;
pub const EVENT: &str = "history";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `docker run` (form, preset, paste).
    Run,
    /// `docker pull`.
    Pull,
    /// Throwaway shell in an image.
    Shell,
    /// `docker exec` shell into a running container.
    Exec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: u64,
    /// Unix milliseconds.
    pub at: u64,
    pub kind: Kind,
    /// The equivalent CLI command, for display and copy.
    pub command: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub request: Option<RunRequest>,
    #[serde(default)]
    pub aws_profile: Option<String>,
    /// `None` while unknown (interactive sessions).
    #[serde(default)]
    pub ok: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Entry {
    pub fn new(kind: Kind, command: String) -> Self {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        Self {
            id: 0,
            at,
            kind,
            command,
            image: None,
            container: None,
            request: None,
            aws_profile: None,
            ok: None,
            error: None,
        }
    }

    pub fn outcome<T>(mut self, result: &Result<T>) -> Self {
        self.ok = Some(result.is_ok());
        self.error = result.as_ref().err().map(|e| e.to_string());
        self
    }
}

pub struct History {
    entries: Mutex<Vec<Entry>>,
    path: Option<PathBuf>,
}

impl History {
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            entries: Mutex::new(entries),
            path,
        }
    }

    fn save(&self, entries: &[Entry]) {
        if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(bytes) = serde_json::to_vec(entries) {
                let _ = std::fs::write(path, bytes);
            }
        }
    }

    pub fn list(&self) -> Vec<Entry> {
        self.entries.lock().unwrap().clone()
    }

    fn change(&self, f: impl FnOnce(&mut Vec<Entry>)) -> Vec<Entry> {
        let mut entries = self.entries.lock().unwrap();
        f(&mut entries);
        self.save(&entries);
        entries.clone()
    }

    /// Add `entry` (newest first). Repeating the latest command just moves
    /// it to now instead of adding a duplicate.
    pub fn record(&self, mut entry: Entry) -> Vec<Entry> {
        self.change(|entries| {
            entry.id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
            if entries
                .first()
                .is_some_and(|e| e.kind == entry.kind && e.command == entry.command)
            {
                entries.remove(0);
            }
            entries.insert(0, entry);
            entries.truncate(MAX_ENTRIES);
        })
    }

    pub fn delete(&self, id: u64) -> Vec<Entry> {
        self.change(|entries| entries.retain(|e| e.id != id))
    }

    pub fn clear(&self) -> Vec<Entry> {
        self.change(Vec::clear)
    }
}

/// Record and tell the UI.
pub fn record(app: &AppHandle, entry: Entry) {
    let list = app.state::<History>().record(entry);
    let _ = app.emit(EVENT, list);
}

#[tauri::command]
pub fn history_list(history: tauri::State<'_, History>) -> Vec<Entry> {
    history.list()
}

#[tauri::command]
pub fn history_delete(app: AppHandle, id: u64) -> Vec<Entry> {
    let list = app.state::<History>().delete(id);
    let _ = app.emit(EVENT, &list);
    list
}

#[tauri::command]
pub fn history_clear(app: AppHandle) -> Vec<Entry> {
    let list = app.state::<History>().clear();
    let _ = app.emit(EVENT, &list);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_newest_first_and_collapses_repeats() {
        let h = History::load(None);
        h.record(Entry::new(Kind::Pull, "docker pull a".into()));
        h.record(Entry::new(Kind::Run, "docker run a".into()));
        let list = h.record(Entry::new(Kind::Run, "docker run a".into()));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].command, "docker run a");
        assert_eq!(list[1].command, "docker pull a");
        let id = list[1].id;
        assert_eq!(h.delete(id).len(), 1);
        assert!(h.clear().is_empty());
    }

    #[test]
    fn caps_entries() {
        let h = History::load(None);
        for i in 0..MAX_ENTRIES + 10 {
            h.record(Entry::new(Kind::Pull, format!("docker pull {i}")));
        }
        assert_eq!(h.list().len(), MAX_ENTRIES);
    }
}
