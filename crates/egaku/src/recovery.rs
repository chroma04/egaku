//! Autosave and crash recovery.
//!
//! While a document has unsaved changes, a copy is written every minute to
//! the app's data folder (in the background, from a copy-on-write
//! snapshot, so painting never waits). Saving, discarding or quitting
//! normally removes it. If egaku is killed or crashes, the copy is still
//! there at the next start and is offered back.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub when: SystemTime,
}

pub struct Recovery {
    dir: Option<PathBuf>,
    session: String,
}

impl Recovery {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Recovery {
            dir: eframe::storage_dir("egaku").map(|d| d.join("recovery")),
            session: format!("{}-{nanos}", std::process::id()),
        }
    }

    fn own(&self) -> Option<(PathBuf, PathBuf)> {
        let d = self.dir.as_ref()?;
        Some((
            d.join(format!("{}.ora", self.session)),
            d.join(format!("{}.txt", self.session)),
        ))
    }

    /// Recovery files left behind by earlier sessions.
    pub fn leftovers(&self) -> Vec<Entry> {
        let Some(dir) = &self.dir else { return vec![] };
        let Ok(rd) = std::fs::read_dir(dir) else {
            return vec![];
        };
        let mut v: Vec<Entry> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ora"))
            .filter(|p| !p.file_stem().is_some_and(|s| s.to_string_lossy() == self.session))
            .map(|p| {
                let (_, name) = self.meta_of(&p);
                let when = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                Entry { path: p, name, when }
            })
            .collect();
        v.sort_by(|a, b| b.when.cmp(&a.when));
        v
    }

    /// Where this session's autosave goes (writing its description too).
    pub fn target(&self, name: &str, original: Option<&Path>) -> Option<PathBuf> {
        let (ora, txt) = self.own()?;
        std::fs::create_dir_all(ora.parent()?).ok()?;
        let meta = format!(
            "{name}\n{}",
            original.map(|p| p.display().to_string()).unwrap_or_default()
        );
        std::fs::write(txt, meta).ok()?;
        Some(ora)
    }

    /// Removes this session's autosave (after a save or a clean exit).
    pub fn clear(&self) {
        if let Some((ora, txt)) = self.own() {
            let _ = std::fs::remove_file(ora);
            let _ = std::fs::remove_file(txt);
        }
    }

    /// The original path and display name of a recovery file.
    pub fn meta_of(&self, ora: &Path) -> (Option<PathBuf>, String) {
        let txt = ora.with_extension("txt");
        let s = std::fs::read_to_string(txt).unwrap_or_default();
        let mut lines = s.lines();
        let name = lines.next().filter(|n| !n.is_empty()).unwrap_or("Untitled").to_string();
        let path = lines
            .next()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ora")));
        (path, name)
    }

    pub fn remove_meta(&self, ora: &Path) {
        let _ = std::fs::remove_file(ora.with_extension("txt"));
    }

    pub fn discard(&self, e: &Entry) {
        let _ = std::fs::remove_file(&e.path);
        self.remove_meta(&e.path);
    }
}
