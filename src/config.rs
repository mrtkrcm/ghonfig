//! The config file, edited in place.
//!
//! This is a line model, not a key/value map, because the file is hand-written
//! and the comments in it are worth more than the settings — they record *why*
//! a value is what it is. A tool that round-trips through a map silently eats
//! them, so every line it does not touch is preserved byte for byte.

use crate::ghostty::{split_setting, Ghostty};
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

pub enum Entry {
    /// Comments, blank lines, anything not a `key = value`.
    Raw(String),
    Setting { key: String, value: String },
}

pub struct ConfigFile {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub dirty: bool,
}

impl ConfigFile {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let entries = text
            .lines()
            .map(|l| match split_setting(l) {
                Some((key, value)) => Entry::Setting { key, value },
                None => Entry::Raw(l.to_string()),
            })
            .collect();
        Ok(Self { path, entries, dirty: false })
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for e in &self.entries {
            match e {
                Entry::Raw(l) => s.push_str(l),
                Entry::Setting { key, value } if value.is_empty() => {
                    s.push_str(&format!("{key} ="))
                }
                Entry::Setting { key, value } => s.push_str(&format!("{key} = {value}")),
            }
            s.push('\n');
        }
        s
    }

    /// Line indices at which `key` is assigned, in file order.
    pub fn positions(&self, key: &str) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Entry::Setting { key: k, .. } if k == key => Some(i),
                _ => None,
            })
            .collect()
    }

    /// The assignment Ghostty actually honours.
    ///
    /// For an ordinary key the last assignment wins. For a repeatable key the
    /// assignments build a *list*: an empty assignment clears it, and the FIRST
    /// survivor is the value in effect — later ones are fallbacks. Reading these
    /// with "last wins" is what made the old tool report a font that was
    /// installed, configured, and not being rendered.
    pub fn effective(&self, key: &str, repeatable: bool) -> Option<usize> {
        let mut hits = self.positions(key);
        if !repeatable {
            return hits.pop();
        }
        let last_reset = hits.iter().rposition(|&i| self.value_at(i).is_empty());
        let start = last_reset.map(|p| p + 1).unwrap_or(0);
        hits.get(start).copied()
    }

    pub fn value_at(&self, idx: usize) -> &str {
        match &self.entries[idx] {
            Entry::Setting { value, .. } => value,
            Entry::Raw(l) => l,
        }
    }

    pub fn get(&self, key: &str, repeatable: bool) -> Option<&str> {
        self.effective(key, repeatable).map(|i| self.value_at(i))
    }

    pub fn set_at(&mut self, idx: usize, value: &str) {
        if let Entry::Setting { value: v, .. } = &mut self.entries[idx] {
            *v = value.to_string();
            self.dirty = true;
        }
    }

    /// Edit the assignment in effect, or append a new one if the key is unset.
    pub fn set(&mut self, key: &str, value: &str, repeatable: bool) {
        match self.effective(key, repeatable) {
            Some(i) => self.set_at(i, value),
            None => {
                if !matches!(self.entries.last(), Some(Entry::Raw(l)) if l.trim().is_empty()) {
                    self.entries.push(Entry::Raw(String::new()));
                }
                self.entries.push(Entry::Setting {
                    key: key.to_string(),
                    value: value.to_string(),
                });
                self.dirty = true;
            }
        }
    }

    pub fn remove_at(&mut self, idx: usize) {
        self.entries.remove(idx);
        self.dirty = true;
    }

    /// Remove every assignment of a key — back to Ghostty's default.
    pub fn unset(&mut self, key: &str) -> usize {
        let n = self.positions(key).len();
        self.entries.retain(|e| !matches!(e, Entry::Setting { key: k, .. } if k == key));
        self.dirty |= n > 0;
        n
    }

    /// Validate, then replace the file atomically, keeping one backup.
    ///
    /// Validation is a hard gate rather than a warning: an invalid line is
    /// skipped by Ghostty with no error anywhere, so writing one is
    /// indistinguishable from writing a setting that had no visible effect.
    pub fn save(&mut self, gh: Option<&Ghostty>) -> Result<()> {
        let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let tmp = dir.join(format!(".{}.{}.{nonce}.ghonfig-tmp", file_name(&self.path), std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // The temporary file contains the same private data as the config.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).context("creating temporary config")?;
        let result = (|| -> Result<()> {
            file.write_all(self.text().as_bytes())?;
            if let Some(gh) = gh {
                gh.validate(&tmp).map_err(anyhow::Error::msg)?;
            }
            self.backup(&tmp)?;
            std::fs::rename(&tmp, &self.path).context("replacing config")?;
            Ok(())
        })();
        // Remove only the file we successfully created, on every failure path.
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result?;
        self.dirty = false;
        Ok(())
    }

    fn backup(&self, tmp: &Path) -> Result<()> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).context("reading config permissions"),
        };
        std::fs::set_permissions(tmp, meta.permissions()).context("preserving config permissions")?;
        let backup = self.path.with_file_name(format!("{}.bak", file_name(&self.path)));
        std::fs::copy(&self.path, backup).context("backing up config")?;
        Ok(())
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "config".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ConfigFile {
        let entries = text
            .lines()
            .map(|l| match split_setting(l) {
                Some((key, value)) => Entry::Setting { key, value },
                None => Entry::Raw(l.to_string()),
            })
            .collect();
        ConfigFile { path: PathBuf::from("/tmp/x"), entries, dirty: false }
    }

    #[test]
    fn comments_survive_an_edit() {
        let mut c = parse("# why 10\nfont-size = 10\n\n# keep me\ntheme = Nord");
        c.set("font-size", "13", false);
        assert_eq!(c.text(), "# why 10\nfont-size = 13\n\n# keep me\ntheme = Nord\n");
    }

    #[test]
    fn ordinary_key_is_last_wins() {
        let c = parse("theme = Nord\ntheme = Dracula");
        assert_eq!(c.get("theme", false), Some("Dracula"));
    }

    #[test]
    fn repeatable_key_is_first_wins_after_the_last_reset() {
        // The failure this encodes: Menlo rendered every character while the
        // tool reported Agave, because it read the tail of the fallback chain.
        let c = parse("font-family = Menlo\nfont-family = Agave");
        assert_eq!(c.get("font-family", true), Some("Menlo"));

        let c = parse("font-family = Menlo\nfont-family =\nfont-family = Agave");
        assert_eq!(c.get("font-family", true), Some("Agave"));
    }

    #[test]
    fn setting_an_absent_key_appends_once() {
        let mut c = parse("theme = Nord");
        c.set("font-size", "12", false);
        assert_eq!(c.text(), "theme = Nord\n\nfont-size = 12\n");
        c.set("font-size", "14", false);
        assert_eq!(c.text(), "theme = Nord\n\nfont-size = 14\n");
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_file_mode_and_handles_backup_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("ghonfig-mode-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config");
        std::fs::write(&path, "theme = Nord\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut c = ConfigFile::load(&path).unwrap();
        c.set("font-size", "12", false);
        c.save(None).unwrap();

        assert!(!c.dirty);
        assert_eq!(std::fs::read_to_string(dir.join("config.bak")).unwrap(), "theme = Nord\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rename must not reset the mode to the umask default");
        // A directory at the backup path forces copy to fail.
        std::fs::remove_file(dir.join("config.bak")).unwrap();
        std::fs::create_dir(dir.join("config.bak")).unwrap();
        let saved = c.text();
        c.set("font-size", "14", false);
        assert!(c.save(None).is_err());
        assert!(c.dirty, "failed saves must keep pending edits");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "temporary file leaked");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unset_removes_every_occurrence() {
        let mut c = parse("palette = 0=#000\npalette = 1=#111\ntheme = Nord");
        assert_eq!(c.unset("palette"), 2);
        assert_eq!(c.text(), "theme = Nord\n");
    }
}
