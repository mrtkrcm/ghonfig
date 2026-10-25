# ghonfig

A TUI for configuring [Ghostty](https://ghostty.org), built with
[ratatui](https://ratatui.rs).

```
  ghonfig  ? for keys                                        ~/.config/ghostty/config
 All 200 · Set 55 · Check

╭ 12 pinned · 200 ────────────────────╮╭ theme ─────────────────────────────────────╮
│ ★ common                            ▐│ themes  463 installed — ⏎ to preview       │
│ ● theme        Nocturnal Winter   ▸ ▐│                                            │
│ ● font-family  MonaspiceNe NFM    ▸ ▐│ A theme to use. This can be the name of a  │
│ ● font-size    10                   ▐│ theme built in to Ghostty, the name of a   │
│ ● cursor-style block              ▸ ▐│ custom theme, or an absolute path…         │
│ ── all settings                     ││                                            │
╰─────────────────────────────────────╯╰────────────────────────────────────────────╯

  ⇥ tab  / search  ⏎ edit  d unset  s save  ? keys  q apply & quit
```

`ghostty +show-config --default --docs` describes all 200 configuration keys —
their defaults and their full documentation. `ghonfig` reads that, puts your
values beside it, and lets you edit them. There is no hardcoded list of keys,
themes, fonts, colours or valid values anywhere in this repository, which is the
point: the tool it replaced kept hand-written arrays of them, and they drifted
silently, twice.

A `▸` marks a key Ghostty can enumerate, so `⏎` opens a list rather than a blank
field — themes with their palettes drawn, font families with the styles they
ship, the 537 colour names, and the exact valid values of all 48 enum keys. A
`●` marks a key your file actually assigns, as opposed to one showing Ghostty's
default — and inside a picker it marks the value you are already on, which is
the only way back to it once a filter has moved the cursor. It opens on the
**All** tab with the keys most people reach for — theme, fonts, opacity,
cursor, padding — pinned above the rest.

## Install

```sh
cargo install --path .     # → ~/.cargo/bin/ghonfig
```

## Use

Run `ghonfig` with no arguments for the TUI.

| Key | |
|---|---|
| `?` | the key list, on screen |
| `⇥` / `←` `→` | switch between **All**, **Set** and **Check** |
| `↑` `↓` / `j` `k` | move; `^D` / `^U` by half a page, `g` / `G` for the ends |
| `⇞` `⇟` | scroll the documentation pane |
| `/` | search — fuzzy on the key name, so `wpx` finds `window-padding-x` |
| `⏎` | edit — a picker wherever Ghostty knows the values, a text field otherwise |
| `^U` *(in a field)* | clear it; the cursor otherwise starts at the end, so typing appends |
| *type* *(in a picker)* | filter; for fonts and colours the text itself is also a legal value |
| `d` | unset — back to Ghostty's default |
| `s` | save now (validated first; refuses to write a config Ghostty would reject) |
| `R` | drop the cached font/theme/colour queries |
| `q` | apply changes and quit (`Q` / `^C` discard without writing) |

Everything is also a subcommand, for scripts and Raycast:

```sh
ghonfig status                    # theme, font, what is actually rendering
ghonfig get font-family
ghonfig set theme "Nocturnal Winter"
ghonfig unset background-blur
ghonfig doc scrollback-limit      # Ghostty's own docs for one key
ghonfig check                     # exits non-zero on a real problem
ghonfig fonts                     # families, tab, the styles each one ships
ghonfig themes
ghonfig colors                    # every colour name Ghostty accepts
ghonfig edit                      # $EDITOR, then validate
```

`ghonfig doc <key>` prints an `accepts:` line naming the key's valid values —
the part that otherwise means a trip to the website.

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

**Ghostty is the schema.** Themes come from `+list-themes` and their palettes
from the theme files it ships, fonts and their styles from `+list-fonts`,
colour names from `+list-colors`, keybind actions from `+list-actions`, and
defaults and docs from `+show-config --default --docs`. Installing a font is
enough to make it selectable.

**Every key's type is derived, in two calls.** `+validate-config` names a key's
valid values when you hand it an invalid one — and it reports on *every* line of
a config in a single pass. So one file that assigns a sentinel to all 200 keys
costs ~90ms and comes back with the valid-value list for all 48 enums at once,
rather than 200 process spawns. Colours need a second pass: Ghostty rejects the
sentinel for `background` and for `font-size` identically, but `background`
accepts `#123456` and `font-size` does not, so *constrained and takes a hex
triple* is the definition of a colour key. That derivation is why there is no
list of colour keys in the source — and why `mouse-shift-capture` correctly
offers `false, true, always, never` instead of the two values a "the default
looks like a bool" shortcut would have inferred.

**Cached, because the truth is slow.** `+list-fonts` takes ~9.6s on this host
and `+list-themes` ~2.4s, so both are cached under `~/.cache/ghonfig`,
invalidated by the Ghostty binary's mtime or after a day. Nothing blocks
startup: the pickers fill in behind a spinner.

## Tests

```sh
cargo test                      # parsing, key semantics, file round-trip, mode preservation
python3 tests/tui_e2e.py        # drives the binary in a pty; needs `pip install pyte`
GHONFIG_BIN=./target/release/ghonfig python3 tests/tui_e2e.py   # against a build, not the install
```

The e2e suite exists because the unit tests cannot see the part that matters:
that a keypress reaches the file, that an invalid value never does, and that
the file comes back with its comments and its `0600` intact. It runs against a
throwaway config via `GHOSTTY_CONFIG`.

## Built with

[ratatui](https://ratatui.rs) 0.30 · `Tabs`, `List`, `Scrollbar`, `Paragraph`,
`Block`, `Clear`, nested `Layout` for the picker's preview column ·
[ratatui-textarea](https://crates.io/crates/ratatui-textarea) for the value
editor · [throbber-widgets-tui](https://crates.io/crates/throbber-widgets-tui)
for the cold-cache spinner.

The theme swatches and colour blocks are ordinary `Span`s in `Color::Rgb`, so
they show true colour in the terminal you are already sitting in. Fonts get no
such preview on purpose: this terminal draws in the font it was launched with,
so anything shown would be a lie — the picker lists the styles a family ships
instead, and applying it asks Ghostty whether the family really renders.
