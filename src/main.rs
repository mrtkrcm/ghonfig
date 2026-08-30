//! ghonfig — configure Ghostty.
//!
//! One config file, edited in place, validated before it is written, with the
//! valid values and documentation coming from the Ghostty binary rather than
//! from anything maintained by hand in this repository.

mod check;
mod config;
mod ghostty;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::ConfigFile;
use ghostty::{Ghostty, Schema};

#[derive(Parser)]
#[command(name = "ghonfig", version, about = "A TUI for configuring Ghostty")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the value Ghostty actually honours for a key
    Get { key: String },
    /// Set a key, validating before the file is written
    Set { key: String, value: Vec<String> },
    /// Remove every assignment of a key, back to Ghostty's default
    Unset { key: String },
    /// Summary of the live configuration
    Status,
    /// Check the configuration for the failures Ghostty does not report
    Check,
    /// Print Ghostty's documentation for a key
    Doc { key: String },
    /// List installed monospace font families
    Fonts,
    /// List available themes
    Themes,
    /// Open the config in $EDITOR, then validate it
    Edit,
    /// Drop the cached font/theme/schema queries
    Refresh,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let gh = Ghostty::find();
    let path = ghostty::config_path();
    let mut cfg = ConfigFile::load(&path)?;
    let schema = || gh.as_ref().and_then(|g| g.schema().ok()).map(Schema::new);

    match cli.cmd {
        None => {
            let mut term = ratatui::init();
            let res = tui::App::new(gh, cfg).run(&mut term);
            ratatui::restore();
            res?;
        }

        Some(Cmd::Get { key }) => {
            let rep = schema().is_some_and(|s| s.is_repeatable(&key));
            match cfg.get(&key, rep) {
                Some(v) => println!("{v}"),
                None => {
                    // Not set here is not the same as having no value.
                    match schema().and_then(|s| s.get(&key).map(|s| s.default_display())) {
                        Some(d) => println!("{d}\t(Ghostty default, not set in your config)"),
                        None => std::process::exit(1),
                    }
                }
            }
        }

        Some(Cmd::Set { key, value }) => {
            let value = value.join(" ");

            // Checked BEFORE the write. Ghostty accepts any string as a font
            // family and falls back to its embedded default in silence, so
            // `+validate-config` cannot catch this and a warning after the fact
            // just means the config now claims a font that never renders.
            // "Cannot tell" is not "missing": this must stay usable on a host
            // where Ghostty is not installed.
            if key.starts_with("font-family") && !value.is_empty() {
                if let Some(g) = &gh {
                    match g.font_renderable(&value) {
                        ghostty::FontStatus::Missing => {
                            eprintln!("error: font family not installed: {value}");
                            eprintln!("Ghostty would accept this and silently render something else.");
                            eprintln!("See `ghonfig fonts` for what is available.");
                            std::process::exit(1);
                        }
                        ghostty::FontStatus::Unknown => {
                            eprintln!("warning: cannot verify that '{value}' renders");
                        }
                        ghostty::FontStatus::Renderable => {}
                    }
                }
            }

            let rep = schema().is_some_and(|s| s.is_repeatable(&key));
            cfg.set(&key, &value, rep);
            cfg.save(gh.as_ref())?;
            println!("{key} = {value}");
            println!("Reload Ghostty: Cmd+Shift+,");
        }

        Some(Cmd::Unset { key }) => {
            let n = cfg.unset(&key);
            if n == 0 {
                println!("{key} was not set");
            } else {
                cfg.save(gh.as_ref())?;
                println!("removed {n} assignment(s) of {key}");
            }
        }

        Some(Cmd::Status) => status(gh.as_ref(), schema().as_ref(), &cfg),

        Some(Cmd::Check) => {
            let checks = check::run(gh.as_ref(), schema().as_ref(), &cfg);
            let failed = checks.iter().filter(|c| !c.ok).count();
            for c in &checks {
                println!("{} {}", if c.ok { "✓" } else { "✗" }, c.title);
                for d in &c.detail {
                    println!("    {d}");
                }
            }
            println!();
            println!("{} passed, {failed} failed", checks.len() - failed);
            if failed > 0 {
                std::process::exit(1);
            }
        }

        Some(Cmd::Doc { key }) => match schema().and_then(|s| s.get(&key).map(|s| (s.default_display(), s.doc.clone()))) {
            Some((default, doc)) => {
                println!("{key}\n");
                if !default.is_empty() {
                    println!("default: {default}\n");
                }
                println!("{doc}");
            }
            None => {
                eprintln!("no such key: {key}");
                std::process::exit(1);
            }
        },

        Some(Cmd::Fonts) => {
            for f in ghostty::require()?.fonts()? {
                println!("{f}");
            }
        }

        Some(Cmd::Themes) => {
            for t in ghostty::require()?.themes()? {
                println!("{t}");
            }
        }

        Some(Cmd::Edit) => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let mut parts = editor.split_whitespace();
            let bin = parts.next().unwrap_or("vi");
            std::process::Command::new(bin)
                .args(parts)
                .arg(&path)
                .status()?;
            if let Some(g) = &gh {
                match g.validate(&path) {
                    Ok(()) => println!("✓ config validates"),
                    Err(e) => {
                        eprintln!("✗ config is INVALID — Ghostty will skip these lines:\n{e}");
                        std::process::exit(1);
                    }
                }
            }
        }

        Some(Cmd::Refresh) => {
            if let Some(g) = &gh {
                g.clear_cache();
            }
            println!("caches cleared");
        }
    }
    Ok(())
}

fn status(gh: Option<&Ghostty>, schema: Option<&Schema>, cfg: &ConfigFile) {
    let rep = |k: &str| schema.is_some_and(|s| s.is_repeatable(k));
    let show = |k: &str| cfg.get(k, rep(k)).unwrap_or("(Ghostty default)");

    println!("Config     {}", cfg.path.display());
    println!("Theme      {}", show("theme"));
    println!("Font       {}", show("font-family"));
    if let Some(face) = gh.and_then(|g| g.rendered_face()) {
        // The configured value and the rendered face are different questions,
        // and the gap between them is where every font bug here has lived.
        println!("Rendering  {face}");
    }
    println!("Size       {}", show("font-size"));
    println!(
        "Settings   {} assigned",
        cfg.entries
            .iter()
            .filter(|e| matches!(e, config::Entry::Setting { .. }))
            .count()
    );
}
