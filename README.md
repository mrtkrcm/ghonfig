# ghonfig

A TUI for configuring [Ghostty](https://ghostty.org), built with
[ratatui](https://ratatui.rs).

```
  ghonfig                                                    ~/.config/ghostty/config
 Set 55 · All 200 · Check

╭ 55 ─────────────────────────────────╮╭ font-thicken ──────────────────────────────╮
│ font-thicken            true        ▐│ default  false                             │
│ font-variation          wght=450    ▐│                                            │
│ adjust-cell-height      2           ▐│ Draw fonts with a thicker stroke, if       │
│ adjust-cell-width       0           ▐│ supported. This is currently only          │
│ window-decoration       true        ▐│ supported on macOS.                        │
╰─────────────────────────────────────╯╰────────────────────────────────────────────╯

  ⇥ tab  / search  ⏎ edit  d unset  s save  q quit
```

`ghostty +show-config --default --docs` describes all 200 configuration keys —
their defaults and their full documentation. `ghonfig` reads that, puts your
values beside it, and lets you edit them. There is no hardcoded list of keys,
themes, or fonts anywhere in this repository, which is the point: the tool it
replaced kept hand-written arrays of both, and they drifted silently, twice.

## Install

```sh
cargo install --path .     # → ~/.cargo/bin/ghonfig
```

## Use

Run `ghonfig` with no arguments for the TUI.

| Key | |
|---|---|
| `⇥` / `←` `→` | switch between **Set**, **All** and **Check** |
| `↑` `↓` / `j` `k` | move; `g` / `G` for the ends |
| `⇞` `⇟` | scroll the documentation pane |
| `/` | search keys and values |
| `⏎` | edit — a picker for themes, fonts and booleans, a text field otherwise |
| `^U` *(while editing)* | clear the field; the cursor otherwise starts at the end, so typing appends |
| `⇥` *(while editing)* | cycle the valid values, once Ghostty has named them |
| `d` | unset — back to Ghostty's default |
| `s` | save (validated first; refuses to write a config Ghostty would reject) |
| `R` | drop the cached font/theme queries |
| `q` | quit (`Q` discards unsaved edits) |

Everything is also a subcommand, for scripts and Raycast:

```sh
ghonfig status                    # theme, font, what is actually rendering
ghonfig get font-family
ghonfig set theme "Nocturnal Winter"
ghonfig unset background-blur
ghonfig doc scrollback-limit      # Ghostty's own docs for one key
ghonfig check                     # exits non-zero on a real problem
ghonfig fonts | ghonfig themes
ghonfig edit                      # $EDITOR, then validate
```

## What `check` looks for

Ghostty does not fail loudly. An invalid line is skipped, a missing font falls
back, and neither is logged anywhere a human reads — so a misconfiguration
looks exactly like a setting that had no visible effect. Each check exists
because that happened:

- **the config validates** — `cursor-style = line` is not a valid value and sat
  in the config for five months
- **the configured font actually renders**, distinguishing *not installed* from
  *shadowed by an earlier `font-family`* — the two look identical and need
  opposite fixes. `+validate-config` returns 0 for a font that does not exist
- **no key is silently overridden by a later duplicate**
- **`scrollback-limit` is in bytes** — it is bytes per surface at ~12.5
  bytes/cell, and every value under 1 MB here turned out to be a line count
- **the window title has exactly one owner** — Ghostty's `title` shell
  integration rewrites the title on every prompt and quietly beats a shell
  `chpwd` hook that something else parses

## Design notes

**One file.** `~/.config/ghostty/config` is the whole configuration, hand-owned
and hand-commented. This replaced a profile system that merged a profile with a
local override file into a third, generated file — where, because Ghostty
*appends* to repeatable keys like `font-family` rather than replacing them, the
"override" was really a fallback and the profile kept winning.

**The file is a line model, not a map.** Comments outlive values; a tool that
round-trips through a key/value map eats the reasoning and keeps the result.
Lines that are not edited come out byte for byte.

**Ghostty is the schema.** Themes come from `+list-themes`, fonts from
`+list-fonts`, defaults and docs from `+show-config --default --docs`, and a
key's valid values from handing `+validate-config` an invalid one and reading
the complaint. Installing a font is enough to make it selectable.

**Cached, because the truth is slow.** `+list-fonts` takes ~9.6s on this host
and `+list-themes` ~2.4s, so both are cached under `~/.cache/ghonfig`,
invalidated by the Ghostty binary's mtime or after a day. Nothing blocks
startup: the pickers fill in behind a spinner.

## Tests

```sh
cargo test                      # parsing, key semantics, file round-trip, mode preservation
python3 tests/tui_e2e.py        # drives the binary in a pty; needs `pip install pyte`
```

The e2e suite exists because the unit tests cannot see the part that matters:
that a keypress reaches the file, that an invalid value never does, and that
the file comes back with its comments and its `0600` intact. It runs against a
throwaway config via `GHOSTTY_CONFIG`.

## Built with

[ratatui](https://ratatui.rs) · `Tabs`, `List`, `Scrollbar`, `Paragraph` ·
[ratatui-textarea](https://crates.io/crates/ratatui-textarea) for the value
editor · [throbber-widgets-tui](https://crates.io/crates/throbber-widgets-tui)
for the cold-cache spinner.
