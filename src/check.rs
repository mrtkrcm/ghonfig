//! Health checks for the live configuration.
//!
//! Every check here exists because the failure it catches is *silent*: Ghostty
//! skips a bad line, falls back on a missing font, and honours the wrong end of
//! a repeated key without logging any of it. A configuration that looks right
//! and is not is the only failure mode worth writing a checker for.

use crate::config::ConfigFile;
use crate::ghostty::{self, FontStatus, Ghostty, Schema};

pub struct Check {
    pub ok: bool,
    pub title: String,
    pub detail: Vec<String>,
}

fn ok(title: impl Into<String>) -> Check {
    Check { ok: true, title: title.into(), detail: vec![] }
}

fn bad(title: impl Into<String>, detail: Vec<&str>) -> Check {
    Check {
        ok: false,
        title: title.into(),
        detail: detail.into_iter().map(String::from).collect(),
    }
}

pub fn run(gh: Option<&Ghostty>, schema: Option<&Schema>, cfg: &ConfigFile) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(if cfg.path.is_file() {
        ok(format!("config exists ({})", cfg.path.display()))
    } else {
        bad("config file does not exist", vec!["Ghostty is running entirely on defaults."])
    });

    let Some(gh) = gh else {
        // Saying so out loud, rather than skipping quietly: a skipped check and
        // a passed check look identical in a results line, and that is the whole
        // failure mode this file exists to close.
        out.push(bad(
            "ghostty binary not found — content checks SKIPPED, not passed",
            vec!["Install Ghostty, or set GHOSTTY_APP_BIN."],
        ));
        return out;
    };

    out.push(match gh.validate(&cfg.path) {
        Ok(()) => ok("config validates"),
        Err(msg) => Check {
            ok: false,
            title: "config is INVALID — Ghostty is silently ignoring these lines".into(),
            detail: msg.lines().map(String::from).collect(),
        },
    });

    out.push(font_check(gh, cfg, schema));
    out.push(scrollback_check(cfg));
    out.push(duplicate_check(cfg, schema));
    out.push(title_owner_check(cfg));
    out
}

/// A font that is missing and a font that is shadowed present identically —
/// configured value ≠ rendered value — but need opposite fixes.
fn font_check(gh: &Ghostty, cfg: &ConfigFile, schema: Option<&Schema>) -> Check {
    let repeatable = schema.is_none_or(|s| s.is_repeatable("font-family"));
    let Some(want) = cfg.get("font-family", repeatable) else {
        return match gh.rendered_face() {
            Some(f) => ok(format!("font: none pinned, using Ghostty's default ({f})")),
            None => ok("font: none pinned"),
        };
    };
    let Some(got) = gh.rendered_face() else {
        return bad("font: could not ask Ghostty what it renders", vec![]);
    };
    if ghostty::face_matches(&got, want) {
        return ok(format!("font renders as configured ({want})"));
    }
    match gh.font_renderable(want) {
        FontStatus::Renderable => bad(
            format!("font-family '{want}' is renderable but Ghostty renders '{got}'"),
            vec!["It is shadowed: an earlier font-family entry wins the fallback chain.",
                 "The FIRST font-family in the file is the primary face, not the last."],
        ),
        FontStatus::Missing => bad(
            format!("font-family is '{want}' but Ghostty renders '{got}' — not installed"),
            vec!["Ghostty accepts any string here and falls back without a word."],
        ),
        FontStatus::Unknown => bad(
            format!("font-family is '{want}' but Ghostty renders '{got}' — cause undetermined"),
            vec![],
        ),
    }
}

/// `scrollback-limit` is BYTES per surface, at ~12.5 bytes/cell — so capacity
/// scales with terminal *width*. Five profiles here once carried values that
/// read as line counts: 10000 was about four lines at 200 columns.
fn scrollback_check(cfg: &ConfigFile) -> Check {
    let Some(v) = cfg.get("scrollback-limit", false) else {
        return ok("scrollback-limit: unset (Ghostty default, 10 MB)");
    };
    match v.parse::<u64>() {
        Ok(n) if n < 1_000_000 => bad(
            format!("scrollback-limit = {n} is BYTES (~{} lines at 200 cols)", n / 2500),
            vec!["Under 1 MB it is almost certainly a line count in disguise.",
                 "Ghostty's own default is 10000000."],
        ),
        Ok(n) => ok(format!("scrollback-limit = {n} bytes (~{} lines at 200 cols)", n / 2500)),
        Err(_) => bad(format!("scrollback-limit = {v} is not a number"), vec![]),
    }
}

/// A non-repeatable key assigned twice: the earlier line looks live and is dead.
fn duplicate_check(cfg: &ConfigFile, schema: Option<&Schema>) -> Check {
    let Some(schema) = schema else {
        return ok("duplicate keys: not checked (no schema)");
    };
    let mut dupes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for e in &cfg.entries {
        let crate::config::Entry::Setting { key, .. } = e else { continue };
        if !seen.insert(key.clone()) || schema.is_repeatable(key) {
            continue;
        }
        let n = cfg.positions(key).len();
        if n > 1 {
            dupes.push(format!("{key} is assigned {n} times — only the last one applies"));
        }
    }
    if dupes.is_empty() {
        ok("no key is silently overridden by a later duplicate")
    } else {
        Check { ok: false, title: "duplicate assignments".into(), detail: dupes }
    }
}

/// Exactly one of the shell and Ghostty may own the window title. Ghostty's own
/// `title` shell-integration feature rewrites it on every precmd, so with both
/// live Ghostty wins and whatever parses the shell's format degrades silently.
fn title_owner_check(cfg: &ConfigFile) -> Check {
    let hook = ghostty::home().join(".config/shell/features/ghostty.zsh");
    let shell_owns = std::fs::read_to_string(&hook)
        .map(|s| s.contains("_ghostty_set_window_title"))
        .unwrap_or(false);
    let ghostty_owns = !cfg
        .get("shell-integration-features", false)
        .is_some_and(|v| v.contains("no-title"));

    if shell_owns && ghostty_owns {
        bad(
            "both the shell and Ghostty set the window title — Ghostty wins on precmd",
            vec!["Add no-title to shell-integration-features, or drop the chpwd hook."],
        )
    } else {
        ok("window title has exactly one owner")
    }
}
