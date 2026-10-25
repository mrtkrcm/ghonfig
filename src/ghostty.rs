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

/// What a key will accept, as Ghostty itself reports it.
///
/// Nothing here is a list of key names — see `probe_kinds` for how the whole
/// schema is classified in two calls to the binary.
#[derive(Clone, PartialEq, Debug)]
pub enum Kind {
    /// A closed set. Ghostty names the members when it rejects a value.
    Enum(Vec<String>),
    /// `#rrggbb` or one of the 537 names `+list-colors` knows.
    Color,
    /// Constrained but open — a number, a size, a path.
    Scalar,
    /// A compound value with its own grammar: `keybind`, `palette`, `adjust-*`.
    Structured,
    /// Anything at all: `font-family`, `theme`, `command`, `title`.
    Text,
}

/// A single configuration key as Ghostty describes it.
pub struct Setting {
    pub key: String,
    /// Ghostty's default. Repeated keys (`keybind`) carry one entry per default.
    pub defaults: Vec<String>,
    pub doc: String,
    pub kind: Kind,
}

/// An installed family together with the styles `+list-fonts` reports beneath it.
pub struct FontFamily {
    pub name: String,
    pub styles: Vec<String>,
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

    pub fn kind(&self, key: &str) -> Kind {
        self.get(key).map_or(Kind::Text, |s| s.kind.clone())
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
                            kind: Kind::Text,
                        });
                    }
                }
                doc.clear();
            } else {
                doc.clear();
            }
        }
        self.probe_kinds(&mut out);
        Ok(out)
    }

    /// Classify every key in the schema, by asking Ghostty twice.
    ///
    /// `+validate-config` reports *every* offending line in one pass, so a
    /// config that assigns a sentinel to all 200 keys costs ~90ms and comes back
    /// with the valid-value list for all 48 enums at once — versus 200 process
    /// spawns to probe them one at a time.
    ///
    /// Colours need the second pass. Ghostty rejects the sentinel for
    /// `background` and for `font-size` identically ("invalid value"), with no
    /// hint of the type; but `background` accepts `#123456` and `font-size` does
    /// not. A key that is constrained AND takes a hex triple is a colour. That
    /// derivation is why there is no list of colour keys in this file.
    fn probe_kinds(&self, settings: &mut [Setting]) {
        let keys: Vec<&str> = settings.iter().map(|s| s.key.as_str()).collect();
        let mut kinds: HashMap<String, Kind> = HashMap::new();
        for line in self.probe_all(&keys, "__ghonfig_probe__", "kinds").lines() {
            if let Some((k, kind)) = classify(line) {
                kinds.insert(k, kind);
            }
        }
        // Keys ABSENT here took the hex triple.
        let hex_rejected: std::collections::HashSet<String> = self
            .probe_all(&keys, "#123456", "hex")
            .lines()
            .filter_map(|l| classify(l).map(|(k, _)| k))
            .collect();

        for s in settings.iter_mut() {
            s.kind = match kinds.get(&s.key) {
                None => Kind::Text,
                Some(Kind::Scalar) if !hex_rejected.contains(&s.key) => Kind::Color,
                Some(k) => k.clone(),
            };
        }
    }

    /// Assign `value` to every key at once and return what Ghostty complains about.
    fn probe_all(&self, keys: &[&str], value: &str, tag: &str) -> String {
        let path = std::env::temp_dir()
            .join(format!("ghonfig-probe-{tag}-{}.conf", std::process::id()));
        let body: String = keys.iter().map(|k| format!("{k} = {value}\n")).collect();
        if std::fs::write(&path, body).is_err() {
            return String::new();
        }
        let out = self.validate(&path).err().unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        out
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

    /// Installed font *families*, each with the styles it ships.
    ///
    /// `+list-fonts` indents face names beneath each family; only the unindented
    /// lines are what `font-family` takes, and the indented ones — which the
    /// picker shows so you can see a family has no italic before choosing it —
    /// are the family name plus a style suffix.
    ///
    /// "JetBrains Mono" is appended because Ghostty EMBEDS it: it renders
    /// perfectly but never appears in this list, so a membership test against
    /// `+list-fonts` rejects the one font most likely to be wanted.
    pub fn fonts(&self) -> Result<Vec<FontFamily>> {
        let text = self.cached("fonts", &["+list-fonts"])?;
        let mut fams: Vec<FontFamily> = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if !line.starts_with(char::is_whitespace) {
                fams.push(FontFamily { name: line.trim_end().to_string(), styles: vec![] });
                continue;
            }
            let Some(f) = fams.last_mut() else { continue };
            let style = style_of(&f.name, line.trim());
            if !f.styles.contains(&style) {
                f.styles.push(style);
            }
        }
        fams.push(FontFamily {
            name: "JetBrains Mono".to_string(),
            styles: vec!["ships with Ghostty".into()],
        });
        fams.sort_by(|a, b| a.name.cmp(&b.name));
        fams.dedup_by(|a, b| a.name == b.name);
        Ok(fams)
    }

    /// The colour names Ghostty accepts wherever a colour is valid.
    ///
    /// `+list-colors` prints each one twice ("alice blue" and "AliceBlue"); the
    /// spaced spelling is dropped so the picker is not half duplicates.
    pub fn colors(&self) -> Result<Vec<(String, String)>> {
        let text = self.cached("colors", &["+list-colors"])?;
        Ok(text
            .lines()
            .filter_map(split_setting)
            .filter(|(name, _)| !name.contains(' '))
            .collect())
    }

    /// Every action a `keybind` can trigger.
    pub fn actions(&self) -> Result<Vec<String>> {
        let text = self.cached("actions", &["+list-actions"])?;
        Ok(text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
    }

    /// A theme's own `key = value` lines, so its palette can be shown before it
    /// is applied. Themes are ordinary Ghostty config files.
    pub fn theme_colors(&self, name: &str) -> Option<Vec<(String, String)>> {
        let mut dirs = vec![home().join(".config/ghostty/themes")];
        if let Some(r) = self.resources_dir() {
            dirs.push(r.join("themes"));
        }
        let text = dirs.iter().find_map(|d| std::fs::read_to_string(d.join(name)).ok())?;
        Some(text.lines().filter_map(split_setting).collect())
    }

    /// `Ghostty.app/Contents/Resources/ghostty` on macOS, `<prefix>/share/ghostty`
    /// elsewhere — derived from where the binary actually is, not assumed.
    fn resources_dir(&self) -> Option<PathBuf> {
        if let Some(d) = std::env::var_os("GHOSTTY_RESOURCES_DIR") {
            return Some(PathBuf::from(d));
        }
        let up = self.bin.parent()?.parent()?;
        [up.join("Resources/ghostty"), up.join("share/ghostty")]
            .into_iter()
            .find(|p| p.is_dir())
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

/// The style a face adds to its family: `Bold Italic` out of
/// `BlexMono Nerd Font Mono Bold Italic`.
///
/// A plain prefix strip is not enough. Half the Nerd Font packages name the
/// family in full and the faces by initials — family `MonaspiceNe Nerd Font
/// Mono`, face `MonaspiceNe NFM Bold` — so after dropping the tokens the two
/// share, one leading all-caps token is dropped as the abbreviation of the rest
/// of the family name. That is a shape rule, not a list of known style names,
/// which would be one more thing to keep in step with what people install.
fn style_of(family: &str, face: &str) -> String {
    let fam: Vec<&str> = family.split_whitespace().collect();
    let all: Vec<&str> = face.split_whitespace().collect();
    let shared = all.iter().zip(&fam).take_while(|(a, f)| a == f).count();
    let mut rest = &all[shared..];
    if rest
        .first()
        .is_some_and(|t| t.len() > 1 && t.chars().all(|c| c.is_ascii_uppercase()))
    {
        rest = &rest[1..];
    }
    match rest.join(" ") {
        s if s.is_empty() => "Regular".to_string(),
        s => s,
    }
}

/// One complaint from `+validate-config`, read back as a type.
///
/// `/path/config:57:cursor-style: invalid value "x", valid values are: bar, block`
/// `/path/config:23:adjust-cell-width: unknown error error.InvalidFormat`
///
/// The path may itself contain colons, so the key is taken from the *last*
/// colon-separated field before the message rather than by counting fields.
fn classify(line: &str) -> Option<(String, Kind)> {
    let (head, rest, structured) = match line.split_once(": invalid value ") {
        Some((h, r)) => (h, r, false),
        None => (line.split_once(": unknown error ")?.0, "", true),
    };
    let key = head.rsplit(':').next()?.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    if structured {
        return Some((key.to_string(), Kind::Structured));
    }
    let values: Vec<String> = rest
        .split_once("valid values are:")
        .and_then(|(_, list)| list.split(['\n', '\r']).next())
        .map(|list| {
            list.split(',')
                .map(|s| s.trim().trim_end_matches('.').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(match values.is_empty() {
        true => (key.to_string(), Kind::Scalar),
        false => (key.to_string(), Kind::Enum(values)),
    })
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
    fn style_is_what_the_face_adds_to_the_family() {
        assert_eq!(style_of("BlexMono Nerd Font Mono", "BlexMono Nerd Font Mono Bold Italic"), "Bold Italic");
        // The shape that a plain prefix strip leaves as "NFM Bold".
        assert_eq!(style_of("MonaspiceNe Nerd Font Mono", "MonaspiceNe NFM Bold"), "Bold");
        assert_eq!(style_of("Andale Mono", "Andale Mono"), "Regular");
    }

    #[test]
    fn classifies_a_validation_complaint() {
        let enum_line = "/private/tmp/x.conf:57:cursor-style: invalid value \"p\", valid values are: bar, block, underline, block_hollow";
        assert_eq!(
            classify(enum_line),
            Some((
                "cursor-style".into(),
                Kind::Enum(vec!["bar".into(), "block".into(), "underline".into(), "block_hollow".into()])
            ))
        );
        // A bool-looking default that is NOT a bool: the old shortcut of reading
        // choices off `defaults` offered true/false and hid the other two.
        let four = "/x:63:mouse-shift-capture: invalid value \"p\", valid values are: false, true, always, never";
        assert!(matches!(classify(four), Some((_, Kind::Enum(v))) if v.len() == 4));

        assert_eq!(
            classify("/x:12:font-size: invalid value \"p\""),
            Some(("font-size".into(), Kind::Scalar))
        );
        assert_eq!(
            classify("/x:23:adjust-cell-width: unknown error error.InvalidFormat"),
            Some(("adjust-cell-width".into(), Kind::Structured))
        );
        assert_eq!(classify("some unrelated stderr noise"), None);
    }

    #[test]
    fn face_match_tolerates_style_suffix_and_spacing() {
        assert!(face_matches("Menlo Regular", "Menlo"));
        assert!(face_matches("JetBrainsMono Nerd Font Mono", "JetBrains Mono Nerd Font Mono"));
        assert!(!face_matches("JetBrains Mono", "Menlo"));
    }
}
