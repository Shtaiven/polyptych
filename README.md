<p align="center">
  <img src="assets/polyptych.svg" alt="polyptych" width="128" />
</p>

# polyptych

An invisible window for padding layouts in Wayland tiling compositors (COSMIC, sway, Hyprland, niri, …).

The window always shows its **idle look**: a colour (fully transparent by default), optionally with an image over it. When it is **focused**, **hovered** or **floating**, a translucent colour and centred text are drawn over that. Drag it with the left mouse button to move it; right-click to close it.

It is a native Wayland client with no toolkit. It uses about 3.5 MB RSS and 0% CPU when idle. It never allocates a window-sized buffer, except to tile an image smaller than the window.

## Install

```sh
cargo install --path .           # → ~/.cargo/bin/polyptych
pixi global install --force-reinstall --path .   # → ~/.pixi/bin/polyptych
```

The pixi install also adds a "Polyptych" launcher to your application menu (`~/.local/share/applications/polyptych_polyptych.desktop`), defined in `recipe/menu.json` with the icon from `assets/polyptych.svg`.

## Config

On the first run, a commented default config is written to `~/.config/polyptych/polyptych.toml` (or under `$XDG_CONFIG_HOME`). Running windows reload it on save.

```toml
idle_color = "#00000000"   # #RRGGBB or #RRGGBBAA
# idle_image = "spacers"   # PNG/JPEG file or folder, drawn over idle_color
idle_image_fit = "fill"    # zoom | fill | stretch | tile
focus_text = "Spacer, right-click to close"
focus_color = "#00000059"  # drawn over the idle look when focused
font = "sans"              # name, generic family or file path
font_weight = 400          # 100–900, variable fonts only
font_size = 18.0
text_color = "#ffffffff"
```

If the file is invalid, a warning is printed to stderr and the defaults are used until it is fixed.

## Idle look

The idle look is always shown. Focus, hover or floating draws `focus_color` and `focus_text` on top of it, so a transparent `focus_color` lets the idle look show through.

- `idle_color` fills the window.
- `idle_image` is a PNG or JPEG file, recognised by its contents, drawn over `idle_color`. The colour shows through transparent parts of the image and around a `zoom`ed image. If the image can't be loaded, a warning is printed and only the colour is shown.
- If `idle_image` is a folder, each spacer picks a random `.png`, `.jpg` or `.jpeg` from it when it opens. Subfolders aren't searched.
- Paths can be absolute, start with `~/`, or be relative to the config file's folder. So `idle_image = "spacers"` means `~/.config/polyptych/spacers`.
- `idle_image_fit` controls how the image fills the window:
  - `zoom`: scaled to fit inside the window, keeping its aspect ratio.
  - `fill`: scaled to cover the window, cropping the overflow.
  - `stretch`: scaled to the window size exactly.
  - `tile`: repeated at its own pixel size from the top left.

The image is decoded once and stays in memory as raw pixels, which costs width × height × 4 bytes. A 4K wallpaper is about 33 MB, so prefer small images. `zoom`, `fill` and `stretch` are scaled by the compositor at no extra cost. `tile` also needs a window-sized buffer when the image is smaller than the window. Images larger than 8192 px on either side are refused.

Image files aren't watched for changes. A spacer loads its image when it opens and when `idle_image` changes in the config.

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

| Compositor | Spawn                                               | Close all                                         |
| ---------- | --------------------------------------------------- | ------------------------------------------------- |
| COSMIC     | Settings → Keyboard → Custom Shortcuts: `polyptych` | `pkill -x polyptych`                              |
| sway       | `bindsym $mod+s exec polyptych`                     | `bindsym $mod+Shift+s exec pkill -x polyptych`    |
| Hyprland   | `bind = SUPER, S, exec, polyptych`                  | `bind = SUPER SHIFT, S, exec, pkill -x polyptych` |

## How "floating" is detected

Tiling compositors set the xdg_toplevel `tiled_*` states on tiled windows. A window with none of those states, and that isn't maximized or fullscreen, counts as floating. A compositor that never sends tiled states will make the window look floating, so it will always be visible.

## Notes

- COSMIC draws its active-window hint around the spacer while it has focus. Set the hint size to 0 in Settings if that bothers you.
- polyptych sets no real minimum size, but compositors may. COSMIC won't shrink a tile below 360×240 px, whatever the window asks for. Sway's minimum is 100×60.
