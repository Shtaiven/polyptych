# polyptych

An invisible window for padding layouts in Wayland tiling compositors (COSMIC, sway, Hyprland, niri, …).

The window is fully transparent unless it is **focused**, **hovered** or **floating**. In any of those states it shows a translucent background with centred text. Click it to close it.

It is a native Wayland client with no toolkit. It uses about 3.5 MB RSS and 0% CPU when idle, and it never allocates a window-sized buffer.

## Install

```sh
cargo install --path .           # → ~/.cargo/bin/polyptych
pixi global install --path .     # → ~/.pixi/bin/polyptych
```

## Config

On the first run, a commented default config is written to `~/.config/polyptych/polyptych.toml` (or under `$XDG_CONFIG_HOME`). Running windows reload it on save.

```toml
text = "Spacer, click to close"
font = "sans"              # name, generic family or file path
font_weight = 400          # 100–900, variable fonts only
font_size = 18.0
text_color = "#ffffffff"   # #RRGGBB or #RRGGBBAA
background = "#00000059"   # #RRGGBB or #RRGGBBAA
```

If the file is invalid, a warning is printed to stderr and the previous config stays in effect.

## Fonts

No font is bundled. The `font` setting is resolved in this order:

1. A file path (`/path/to/font.ttf` or `~/...`) is loaded directly.
2. `fc-match` is asked, so your fontconfig aliases and preferences apply. Unknown names produce a warning showing what fontconfig substituted.
3. If `fc-match` isn't installed, the XDG font directories are searched (`~/.local/share/fonts`, `~/.fonts`, then `/usr/local/share/fonts` and `/usr/share/fonts`). Matching uses the names stored inside each font file, so `"Inter"`, `"Noto Sans Bold"` and `"DejaVu Sans Mono"` all work. The generic names `sans`, `serif` and `monospace` map to a list of common families.
4. If nothing matches, it falls back to `sans`. If no font can be found at all, the background is drawn without text.

`font_weight` (100–900) applies to variable fonts, limited to the range each font supports. Other fonts are drawn at their own weight; to use a static bold font, give its file path.

The font file is only read while the text is drawn, so it doesn't stay in memory.

## Hotkeys

Bind compositor shortcuts to spawn a spacer and to close them all:

| Compositor | Spawn | Close all |
|---|---|---|
| COSMIC | Settings → Keyboard → Custom Shortcuts: `polyptych` | `pkill -x polyptych` |
| sway | `bindsym $mod+s exec polyptych` | `bindsym $mod+Shift+s exec pkill -x polyptych` |
| Hyprland | `bind = SUPER, S, exec, polyptych` | `bind = SUPER SHIFT, S, exec, pkill -x polyptych` |

## How "floating" is detected

Tiling compositors set the xdg_toplevel `tiled_*` states on tiled windows. A window with none of those states, and that isn't maximized or fullscreen, counts as floating. A compositor that never sends tiled states will make the window look floating, so it will always be visible.

## Notes

- COSMIC draws its active-window hint around the spacer while it has focus. Set the hint size to 0 in Settings if that bothers you.
