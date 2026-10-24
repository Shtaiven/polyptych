# polyptych: spacer window (Rust) plan

## Context
The goal is an invisible "spacer" window for padding layouts in Wayland tiling compositors (COSMIC first, plus sway, Hyprland and niri; Wayland only). It must stay simple and use minimal RAM and CPU.
- The window is fully transparent unless it is focused, hovered or floating.
- In any of those states it shows a translucent background with centred text, "Spacer, click to close" by default.
- A click closes it.
- The text, text colour and background colour/alpha are configurable in a TOML file that running windows reload live. There is no GUI.
- It must install with both plain `cargo install` and `pixi global install`.

The user chose Rust with smithay-client-toolkit (SCTK) over Python + GTK for footprint.

Environment: Fedora, COSMIC on Wayland, cargo 1.99 and pixi 0.82.1 (both from `~/.pixi/bin`). pixi supports `pixi global install --path <dir>`.

**App name: `polyptych`.** Derive it from a single source: `const APP: &str = env!("CARGO_PKG_NAME")`. That makes a rename a change to `Cargo.toml` and `pixi.toml` only. The binary name, Wayland app_id, title, config dir and file name, and the `pkill` target all follow `APP`.

## Step 0: create the project
- Create a new folder: `mkdir ~/Projects/polyptych && cargo init --bin --name polyptych ~/Projects/polyptych`. This also runs `git init`.
- Copy this plan to `~/Projects/polyptych/PLAN.md`.
- All work happens inside that folder.

## Project layout: `/home/steven/Projects/polyptych/`
- `Cargo.toml`
  - Edition 2024.
  - Dependencies:
    - `smithay-client-toolkit` 0.20.x with its `calloop` feature.
    - `calloop-wayland-source`.
    - `fontdue`.
    - `toml` with `default-features = false` and features `parse` + `serde`.
    - `serde` with `derive`.
    - `rustix` with `fs`, for inotify. It is already a transitive dependency.
  - Release profile: `opt-level="s"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip=true`.
- `src/main.rs`: Wayland setup, state, rendering and the event loop (about 250 lines).
- `src/config.rs`: config struct, colour parsing, load-with-defaults and the inotify watcher (about 80 lines).
- `assets/font.ttf`: a static sans TTF from the system (Noto Sans Regular or DejaVu Sans, both licensed for embedding), embedded with `include_bytes!`.
- `pixi.toml`: pixi-build manifest (see Install).
- `README.md`: install, config and hotkey instructions.

## Config: `$XDG_CONFIG_HOME/{APP}/{APP}.toml` (defaults to `~/.config/polyptych/polyptych.toml`)
```toml
text = "Spacer, click to close"
text_color = "#ffffffff"   # #RRGGBB or #RRGGBBAA
background = "#00000059"   # #RRGGBB or #RRGGBBAA (alpha 0x59 ≈ 35%)
```
- Every field is optional and falls back to the defaults above (`#[serde(default)]`).
- On the first run, if the file is missing, create the directory and write this commented default file so the user has something to edit.
- If the TOML is invalid or a colour is bad, print a warning to stderr and keep the last good config, or the defaults.
- Colours are converted to premultiplied ARGB8888 at load time.

## Design (`src/main.rs`)
1. **Globals.** Bind these via SCTK:
   - `CompositorState`, `SubcompositorState`, `XdgShell`, `SeatState`, `ShmState`.
   - `wp_viewporter`, bound manually with a no-op dispatch.
2. **Window.**
   - Create it with `WindowDecorations::RequestClient` and draw none, so there is no title bar.
   - Title and app_id are both `APP`.
   - Don't set min/max size, so it tiles normally. Use 400x300 when the compositor leaves the size to us.
3. **Minimal-memory rendering.** The aim is never to allocate a window-sized buffer.
   - **Main surface:** a 1x1 shm buffer scaled to the window size with `wp_viewport.set_destination(w, h)`. The pixel is transparent when hidden and the configured `background` when shown.
   - **Text subsurface:** a buffer the size of the text, rasterized with fontdue in `text_color` with premultiplied alpha.
     - It is centred with `set_position` on each configure.
     - It has an empty input region.
     - Hide it by attaching a null buffer; show it by re-attaching the cached buffer.
     - Re-rasterize only when the text, colour or integer scale changes. Use `set_buffer_scale` for HiDPI.
   - Total shm is a few tens of KB, regardless of window size.
4. **State:** `focused`, `hovered` and `floating`, with `visible = focused || hovered || floating`. Commit only when `visible`, the size or the config changes.
   - `focused`: the `ACTIVATED` flag in `WindowConfigure.state`.
   - `floating`: none of `TILED_LEFT/RIGHT/TOP/BOTTOM`, `MAXIMIZED` or `FULLSCREEN` is set. These are the standard xdg_toplevel states, sent by COSMIC, sway, Hyprland and niri.
   - `hovered`: pointer Enter/Leave on the main surface.
   - A pointer `Release` or a compositor `close` request exits.
5. **Event loop (calloop).** It has two sources and no timers, so idle CPU is 0%.
   - `WaylandSource` for the Wayland connection.
   - A `Generic` source on a non-blocking inotify fd (`rustix::fs::inotify`).
     - It watches the config directory, not the file, so editors that save by rename are caught.
     - Mask: `CLOSE_WRITE | MOVED_TO | CREATE`, filtered on the file name.
     - On a match: reload the config, re-rasterize the text and commit.

## Install (both paths)
- **Cargo:** `cargo install --path /home/steven/Projects/polyptych`.
- **Pixi:** `pixi.toml` has these sections:
  - `[workspace]`: `preview = ["pixi-build"]`, conda-forge channel, linux-64 and linux-aarch64.
  - `[package]`: name and version.
  - `[package.build]`: `backend = { name = "pixi-build-rust", version = "*" }`.
  - Install with `pixi global install --path /home/steven/Projects/polyptych`.
  - If that path doesn't work in 0.82.1, fall back to a rattler-build `recipe.yaml` and install the built `.conda`.
- Runtime needs only `libwayland-client`. No GTK or system font packages are needed.

## Global "close all" hotkey
This needs no code: bind a compositor shortcut to `pkill -x polyptych`. In COSMIC that's Settings → Keyboard → Custom Shortcuts. The README will also show the sway and Hyprland lines. I won't edit the user's COSMIC config.

## Known limits (to tell the user)
- COSMIC's active-hint border is compositor-drawn and still shows while the spacer has focus.
- The window appears in Alt-Tab.
- A compositor that doesn't send tiled states would make the spacer always look "floating".

## Verification
1. Build with `cargo build --release` and run `./target/release/polyptych` in COSMIC tiling mode.
   - It should be invisible when idle.
   - Hover and focus should both show the background and text.
   - A click should close it.
   - On the first run, `~/.config/polyptych/polyptych.toml` should be created.
2. Toggle floating. The background should stay visible while floating and hide when tiled and unfocused. If not, temporarily log `configure.state`.
3. Live config:
   - With a spacer running, edit the text and colours in the TOML. A hovered or floating spacer should update right away.
   - Save invalid TOML: there should be a warning on stderr and the old look should stay.
   - Saving through an editor that renames on save must also trigger a reload.
4. Footprint: `ps -o rss,pcpu -C polyptych`. Target is under 5 MB RSS and 0.0% CPU when idle.
5. Run several spacers, then `pkill -x polyptych`; all should close.
6. Test both installs: `cargo install --path .` and `pixi global install --path .`, then run `polyptych` from PATH.
