# Ghostty Keybindings & Shell Helpers

Every binding below was taken from `ghostty +list-keybinds --default` on
Ghostty 1.3.1. None of the profiles in this repo define a single `keybind`
line, so these defaults are exactly what is in effect.

> **Regenerate this table** after a Ghostty upgrade:
> ```bash
> /Applications/Ghostty.app/Contents/MacOS/ghostty +list-keybinds --default
> ```
> Ghostty spells the Command key `super`, not `cmd`. Searching the output for
> `cmd+` matches nothing and makes every real binding look unbound.

## Windows, tabs, splits

| Key | Action |
|-----|--------|
| `Cmd+N` | New window |
| `Cmd+T` | New tab |
| `Cmd+W` | Close surface (tab or split) |
| `Cmd+Q` | Quit |
| `Cmd+D` | Split right |
| `Cmd+Shift+D` | Split down |
| `Cmd+Shift+Enter` | Toggle split zoom |
| `Cmd+Ctrl+=` | Equalize splits |

## Navigation

| Key | Action |
|-----|--------|
| `Cmd+[` / `Cmd+]` | Previous / next split |
| `Cmd+Alt+←↑↓→` | Focus split in that direction |
| `Cmd+1` … `Cmd+8` | Go to tab N |
| `Cmd+Shift+[` / `Cmd+Shift+]` | Previous / next tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Cmd+↑` / `Cmd+↓` | Jump to previous / next shell prompt |
| `Cmd+PageUp` / `Cmd+PageDown` | Scroll one page |

There is **no** `Cmd+H/J/K/L` split navigation. `Cmd+J` is `scroll_to_selection`
and `Cmd+K` is `clear_screen`; `Cmd+H` is macOS "hide application" and never
reaches the terminal. Use `Cmd+Alt+arrows` or `Cmd+[` / `Cmd+]`.

## Text & search

| Key | Action |
|-----|--------|
| `Cmd+C` / `Cmd+V` | Copy / paste |
| `Cmd+Shift+V` | Paste from selection |
| `Cmd+A` | Select all |
| `Cmd+K` | Clear screen |
| `Cmd+G` / `Cmd+Shift+G` | Next / previous search match |
| `Cmd+Shift+F` | End search |

## Config & display

| Key | Action |
|-----|--------|
| `Cmd+,` | **Open** config in editor |
| `Cmd+Shift+,` | **Reload** config |
| `Cmd++` / `Cmd+-` / `Cmd+0` | Font bigger / smaller / reset |
| `Cmd+Alt+I` | Toggle inspector |

`Cmd+,` opens and `Cmd+Shift+,` reloads — this pair is easy to get backwards.

`Cmd+,` opens `~/.config/ghostty/config`, which is a **generated file**. Edits
there are discarded by the next `gtheme`, `gfont`, `gt profile` or `ggen`. Edit
the source instead:

- profile — `ghostty-implementation/profiles/<name>.conf`
- machine overrides — `~/.config/ghostty/local-config`

## Shell helpers

Defined in `ghostty-implementation/shell-integration.zsh`, sourced from
`local/zshrc-integration.zsh`.

| Command | Action |
|---------|--------|
| `gthemes` | Interactive theme picker (fzf) |
| `gfonts` | Interactive font picker (fzf) |
| `gprofiles` | Interactive profile picker (fzf) |
| `gtoggle` | Toggle dark/light theme |
| `gshow` / `gstatus` | Show current configuration |
| `ghelp` | List these helpers |
| `gtheme <name>` | Set theme |
| `gfont <name>` | Set font family |
| `gsize <n>` | Set font size |
| `gcycle` / `gcyclefont` | Cycle theme / font |
| `gpreset <name>` | Apply preset: `focus-dark`, `focus-light`, `minimal`, `clear` |
| `gdefault` `gmodern` `gminimal` `gnight` | Switch profile |
| `ggen` | Regenerate the config from profile + local-config |
| `gcheck` | Run `validate-migration.sh` |

`gfont` refuses a family Ghostty cannot render. This matters because Ghostty
does not: `+validate-config` returns 0 for a nonexistent font and the terminal
quietly falls back to its embedded JetBrains Mono. `font-family = SpaceMono
Nerd Font Mono` sat in `local-config` in exactly that state, reported as active
by `gt status`, rendering something else entirely.

Custom bindings go in `~/.config/ghostty/local-config`:

```
keybind = super+shift+p=text:cd ~/code/my-project\r
```

Check for a collision first — `Cmd+G`, `Cmd+Shift+G`, `Cmd+Shift+F` and
`Cmd+Shift+Z` are all bound by default:

```bash
/Applications/Ghostty.app/Contents/MacOS/ghostty +list-keybinds --default | grep 'super+shift+p='
```
