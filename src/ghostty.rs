//! Everything ghonfig knows about Ghostty, it asks Ghostty.
//!
//! The predecessor to this tool kept hand-written arrays of themes and fonts.
//! Both drifted, silently, twice: entries that were not installed rendered
//! Ghostty's embedded fallback while the tool reported the name it had written.
//! So there are no lists in this file — only queries, and a cache, because
//! `+list-fonts` costs 9.6s on this host and a TUI cannot pay that per keystroke.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Fonts and themes change when the *system* changes, not when Ghostty is
/// upgraded, so a binary-mtime stamp alone would never expire. TTL backs it up.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// A single configuration key as Ghostty describes it.
pub struct Setting {
    pub key: String,
    /// Ghostty's default. Repeated keys (`keybind`) carry one entry per default.
    pub defaults: Vec<String>,
    pub doc: String,
}

impl Setting {
    pub fn default_display(&self) -> String {
        match self.defaults.len() {
            0 => String::new(),
            1 => self.defaults[0].clone(),
            n => format!("({n} defaults)"),
        }
    }
}

/// Ghostty's own description of every key, indexed.
pub struct Schema {
    pub settings: Vec<Setting>,
    index: HashMap<String, usize>,
}

impl Schema {
    pub fn new(settings: Vec<Setting>) -> Self {
        let index = settings
            .iter()
            .enumerate()
            .map(|(i, s)| (s.key.clone(), i))
            .collect();
        Self { settings, index }
    }

    pub fn get(&self, key: &str) -> Option<&Setting> {
        self.index.get(key).map(|&i| &self.settings[i])
    }

    /// Whether Ghostty *appends* to this key rather than replacing it.
    ///
    /// Taken from the docs Ghostty ships ("This configuration can be repeated
    /// multiple times…") plus the observable fact that a key with several
    /// defaults is one Ghostty accumulates. A hardcoded list of repeatable keys
    /// would be one more thing to drift out of date with the binary.
    pub fn is_repeatable(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|s| s.defaults.len() > 1 || s.doc.contains("can be repeated"))
    }

    /// Values a key will accept, when that is a closed set.
    pub fn choices(&self, key: &str) -> Option<Vec<String>> {
        let s = self.get(key)?;
        // Booleans never reach the enum probe: Ghostty reports them as an
        // invalid value with no list, so read them off the default instead.
        if s.defaults.len() == 1 && matches!(s.defaults[0].as_str(), "true" | "false") {
            return Some(vec!["true".into(), "false".into()]);
        }
        None
    }
}

/// Result of asking the renderer whether a font family will actually be used.
#[derive(PartialEq, Clone, Copy)]
pub enum FontStatus {
    Renderable,
    Missing,
    /// Could not be determined. Never treat this as `Missing` — refusing a font
    /// because the question was unanswerable is worse than the mistake it avoids.
    Unknown,
}

#[derive(Clone)]
pub struct Ghostty {
    bin: PathBuf,
    cache_dir: PathBuf,
}

impl Ghostty {
    /// On macOS the CLI lives inside the app bundle and is not on PATH, so a
    /// PATH lookup alone finds nothing on the very host this targets.
    pub fn find() -> Option<Self> {
        let bin = which("ghostty").or_else(|| {
            let app = std::env::var("GHOSTTY_APP_BIN").unwrap_or_else(|_| {
                "/Applications/Ghostty.app/Contents/MacOS/ghostty".to_string()
            });
            let p = PathBuf::from(app);
            p.is_file().then_some(p)
        })?;
        Some(Self { bin, cache_dir: cache_dir() })
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new(&self.bin)
            .args(args)
            .output()
            .with_context(|| format!("running {} {:?}", self.bin.display(), args))
    }

    /// Cached stdout of a `+action`, invalidated by binary mtime or TTL.
    fn cached(&self, name: &str, args: &[&str]) -> Result<String> {
        let path = self.cache_dir.join(format!("{name}.txt"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if self.cache_fresh(&path) {
                return Ok(text);
            }
        }
        let out = self.run(args)?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !text.trim().is_empty() {
            let _ = std::fs::create_dir_all(&self.cache_dir);
            let _ = std::fs::write(&path, &text);
        }
        Ok(text)
    }

    fn cache_fresh(&self, path: &Path) -> bool {
        let (Ok(cache), Ok(bin)) = (mtime(path), mtime(&self.bin)) else { return false };
        cache > bin && SystemTime::now().duration_since(cache).unwrap_or(CACHE_TTL) < CACHE_TTL
    }

    pub fn clear_cache(&self) {
        let _ = std::fs::remove_dir_all(&self.cache_dir);
    }

    /// Every configuration key Ghostty has, with its default and documentation.
    ///
    /// This is the whole reason the tool can show 634 settings without a
    /// hand-maintained schema: `+show-config --default --docs` *is* the schema.
    pub fn schema(&self) -> Result<Vec<Setting>> {
        let text = self.cached("schema", &["+show-config", "--default", "--docs"])?;
        let mut out: Vec<Setting> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut doc: Vec<String> = Vec::new();

        for line in text.lines() {
            if let Some(rest) = line.strip_prefix('#') {
                doc.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            } else if let Some((k, v)) = split_setting(line) {
                match index.get(&k) {
                    // A repeated key is one setting with several defaults, not
                    // several settings — `keybind` alone would otherwise be 94 rows.
                    Some(&i) => out[i].defaults.push(v),
                    None => {
                        index.insert(k.clone(), out.len());
                        out.push(Setting {
                            key: k,
                            defaults: if v.is_empty() { vec![] } else { vec![v] },
                            doc: doc.join("\n").trim().to_string(),
                        });
                    }
                }
                doc.clear();
            } else {
                doc.clear();
            }
        }
        Ok(out)
    }

    /// `Name (resources)` / `Name (user)` — strip the source annotation.
    pub fn themes(&self) -> Result<Vec<String>> {
        let text = self.cached("themes", &["+list-themes"])?;
        Ok(text
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                let l = l.strip_suffix(" (resources)").unwrap_or(l);
                let l = l.strip_suffix(" (user)").unwrap_or(l);
                (!l.is_empty()).then(|| l.to_string())
            })
            .collect())
    }

    /// Installed font *families*. `+list-fonts` indents style names beneath each
    /// family; only the unindented lines are what `font-family` takes.
    ///
    /// "JetBrains Mono" is prepended because Ghostty EMBEDS it: it renders
    /// perfectly but never appears in this list, so a membership test against
    /// `+list-fonts` rejects the one font most likely to be wanted.
    pub fn fonts(&self) -> Result<Vec<String>> {
        let text = self.cached("fonts", &["+list-fonts"])?;
        let mut fams: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with(char::is_whitespace))
            .map(|l| l.trim_end().to_string())
            .collect();
        fams.push("JetBrains Mono".to_string());
        fams.sort_unstable();
        fams.dedup();
        Ok(fams)
    }

    /// Ghostty does not fail loudly on a bad setting: it skips the line, keeps
    /// the default, and logs nothing a human reads. This is the only thing
    /// standing between a typo and a setting that silently never applied.
    pub fn validate(&self, path: &Path) -> Result<(), String> {
        let arg = format!("--config-file={}", path.display());
        let out = self
            .run(&["+validate-config", &arg])
            .map_err(|e| e.to_string())?;
        let mut msg = String::from_utf8_lossy(&out.stderr).into_owned();
        msg.push_str(&String::from_utf8_lossy(&out.stdout));
        let msg = msg.trim().to_string();
        if out.status.success() && msg.is_empty() {
            Ok(())
        } else {
            Err(msg)
        }
    }

    /// Ask Ghostty for a key's valid values by handing it an invalid one.
    ///
    /// The error names them: `cursor-style: invalid value "x", valid values
    /// are: bar, block, underline, block_hollow`. Free-form keys accept the
    /// sentinel and answer nothing, which is the correct answer for them.
    pub fn valid_values(&self, key: &str) -> Option<Vec<String>> {
        let probe = std::env::temp_dir().join("ghonfig-probe.conf");
        std::fs::write(&probe, format!("{key} = __ghonfig_probe__\n")).ok()?;
        let err = self.validate(&probe).err()?;
        let _ = std::fs::remove_file(&probe);
        let (_, list) = err.split_once("valid values are:")?;
        let list = list.split(['\n', '\r']).next()?;
        Some(
            list.split(',')
                .map(|s| s.trim().trim_end_matches('.').to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        )
    }

    /// The face Ghostty actually renders with, under the live config.
    pub fn rendered_face(&self) -> Option<String> {
        let out = self.run(&["+show-face", "--string=A", "--style=regular"]).ok()?;
        parse_face(&String::from_utf8_lossy(&out.stdout))
    }

    /// Will Ghostty render with this family?
    ///
    /// `+validate-config` cannot answer this — it returns 0 for
    /// `font-family = Totally Bogus Font 9000` and Ghostty quietly falls back.
    /// Neither can a `+list-fonts` lookup, which omits the embedded JetBrains
    /// Mono. So ask the renderer, under an isolated HOME/XDG_CONFIG_HOME:
    /// `--config-file` *adds* to the default config load rather than replacing
    /// it, which would otherwise mix in the live config.
    pub fn font_renderable(&self, family: &str) -> FontStatus {
        let probe = std::env::temp_dir().join(format!("ghonfig-face-{}", std::process::id()));
        let cfg = probe.join("ghostty");
        if std::fs::create_dir_all(&cfg).is_err() {
            return FontStatus::Unknown;
        }
        let _ = std::fs::write(cfg.join("config"), format!("font-family = {family}\n"));
        let out = Command::new(&self.bin)
            .args(["+show-face", "--string=A", "--style=regular"])
            .env("HOME", &probe)
            .env("XDG_CONFIG_HOME", &probe)
            .output();
        let _ = std::fs::remove_dir_all(&probe);

        let Ok(out) = out else { return FontStatus::Unknown };
        let Some(face) = parse_face(&String::from_utf8_lossy(&out.stdout)) else {
            return FontStatus::Unknown;
        };
        if face_matches(&face, family) {
            FontStatus::Renderable
        } else {
            FontStatus::Missing
        }
    }
}

/// The reported face may carry a style suffix ("Menlo Regular"), and families
/// differ in spacing between the upstream name and the installed one.
pub fn face_matches(face: &str, family: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(|c| c.to_lowercase())
            .collect::<String>()
    };
    norm(face).starts_with(&norm(family))
}

/// Expected form: `U+41 « A » found in face “Some Family Name”.`
fn parse_face(out: &str) -> Option<String> {
    let (_, face) = out.split_once("face ")?;
    let face = face.trim().trim_end_matches('.');
    let face = face.trim_matches(|c| c == '"' || c == '\u{201c}' || c == '\u{201d}');
    (!face.is_empty()).then(|| face.to_string())
}

/// `key = value`, ignoring comments and blanks. Empty values are meaningful
/// (Ghostty clears a repeatable key that way), so they are kept.
pub fn split_setting(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let (k, v) = t.split_once('=')?;
    let k = k.trim();
    if k.is_empty() || k.contains(char::is_whitespace) {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

fn mtime(p: &Path) -> Result<SystemTime> {
    Ok(std::fs::metadata(p)?.modified()?)
}

fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let p = dir.join(cmd);
            p.is_file().then_some(p)
        })
    })
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("ghonfig")
}

/// Where Ghostty reads its configuration. One file, hand-owned.
pub fn config_path() -> PathBuf {
    std::env::var_os("GHOSTTY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config/ghostty/config"))
}

pub fn require() -> Result<Ghostty> {
    Ghostty::find().ok_or_else(|| {
        anyhow!("ghostty binary not found (looked on PATH and in /Applications/Ghostty.app)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_and_ignores_comments() {
        assert_eq!(split_setting("font-size = 10"), Some(("font-size".into(), "10".into())));
        assert_eq!(split_setting("  theme=Nord  "), Some(("theme".into(), "Nord".into())));
        // An empty value is a real assignment: it clears a repeatable key.
        assert_eq!(split_setting("keybind ="), Some(("keybind".into(), "".into())));
        assert_eq!(split_setting("# theme = Nord"), None);
        assert_eq!(split_setting(""), None);
        assert_eq!(split_setting("not a setting"), None);
    }

    #[test]
    fn parses_the_face_report() {
        let out = "U+41 « A » found in face \u{201c}MonaspiceNe Nerd Font Mono\u{201d}.";
        assert_eq!(parse_face(out).as_deref(), Some("MonaspiceNe Nerd Font Mono"));
        assert_eq!(parse_face("nonsense"), None);
    }

    #[test]
    fn face_match_tolerates_style_suffix_and_spacing() {
        assert!(face_matches("Menlo Regular", "Menlo"));
        assert!(face_matches("JetBrainsMono Nerd Font Mono", "JetBrains Mono Nerd Font Mono"));
        assert!(!face_matches("JetBrains Mono", "Menlo"));
    }
}
