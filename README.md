# mouse-control

A minimal macOS menu-bar app that enhances your mouse. It is a stripped-down extract of the
mouse-control features from [oh-my-tab](https://github.com/eacryo/oh-my-tab), keeping only the
mouse functionality with no window switcher and no settings window.

It is pure Rust calling AppKit / CoreGraphics / IOKit directly through `objc2` FFI — no Swift
bridge, no UI framework, no Dock icon.

It ships as a **single unbundled binary** with no `.app` packaging: the binary registers
itself as a launchd LaunchAgent (auto-start at login, restart on crash) and still shows the
menu-bar icon, because the agent runs inside the user's GUI session.

## Features

- **Reverse scrolling** — flip the wheel direction relative to the system's current setting.
- **Line scrolling** — turn each wheel notch into a fixed number of lines.
- **Continuous scrolling** — emit trackpad-style pixel scroll events for apps that behave
  poorly with discrete wheel events (for example iPhone Mirroring).
- **Disable pointer acceleration** — 1:1 linear cursor tracking (macOS 14+ linear-scaling
  switch, legacy fallback on older systems). Original values are restored on disable/quit.
- **Side-button remapping** — map side buttons (3/4) to keyboard shortcuts or media keys
  (e.g. `3 = "ctrl+left"` for browser back). Holding the button presses the target, releasing
  it releases; no auto-repeat.
- **Per-device profiles** — match devices by VID+PID; each device can have its own settings,
  merged over an "All Mice" default profile (more specific profiles win).
- **Per-app overrides** — add `app_bundle_id` or `app_name` to a profile so it only applies
  while that app is frontmost; a combined device + app profile overrides both device-only and
  app-only profiles.
- **Hot toggle & hot reload** — enable/disable from the menu (persisted to config), or edit
  the TOML and choose *Reload Config*; no restart needed.
- **Bluetooth resilience** — device plug/unplug is monitored, so the attribution chain
  recovers automatically after a Bluetooth disconnect/reconnect, and pointer acceleration is
  re-applied.

## Requirements

- macOS 13+ (event-tap behavior verified on recent macOS; older versions best-effort).
- **Accessibility** permission for the app. The HID-level event tap needs it to modify/drop
  events. Without it the app logs a retry notice and keeps running; grant it under
  System Settings → Privacy & Security → Accessibility.

## Build & run

```sh
cargo check   # fast type-check
cargo test    # unit tests (profile resolution + scroll direction)
cargo run     # build + run (takes over the mouse event tap)
```

## Run as a login service

The binary installs itself as a launchd LaunchAgent:

```sh
cargo build --release
./target/release/mouse-control install
```

`install` copies the binary to `~/.local/bin/mouse-control`, ad-hoc signs it, writes
`~/Library/LaunchAgents/com.eacryo.mouse-control.plist`, and bootstraps the agent (an already
loaded agent is unloaded first, so `install` doubles as the redeploy/restart path). Companion
subcommands: `status` prints install/service/signing state, `uninstall` stops the agent and
removes the plist and installed binary (config and logs are kept), `--help` prints usage.
Running the binary with no arguments behaves exactly like before: foreground process with the
menu-bar icon.

The agent loads only in GUI sessions and restarts on crash; a menu Quit exits cleanly and
stays off until the next login or `launchctl kickstart gui/$(id -u)/com.eacryo.mouse-control`.
launchd's stdout/stderr land in `~/Library/Logs/mouse-control/launchd-{stdout,stderr}.log`.

**Accessibility permission.** Whenever the app finds itself untrusted (first launch, or after
`install` redeploys a new build: the ad-hoc signature pins the grant to the build's cdhash, so
the old grant silently stops matching), it raises the system permission prompt, both at launch
and when you enable mouse control from the menu. A stale entry from a previous build still
appears enabled under System Settings → Privacy & Security → Accessibility; remove it, then
grant again from the prompt. The event tap retries for about two minutes, so granting takes
effect without relaunching. `mouse-control status` shows the current designated requirement.

## Configuration

Config lives at `~/.config/mouse-control/config.toml` (auto-created with Windows-style
defaults on first run: reverse scrolling, Line mode, 1:1 tracking, and side-button desktop
switching). The schema:

```toml
[mouse]
enabled = true

[[mouse.profiles]]              # no device fields = "All Mice" default profile
reverse_scroll = true           # Windows-style: reverse relative to the system direction
scroll_mode = "line"            # "default" | "line" | "continuous"
line_count = 3                  # lines per notch in line mode (1..=20)
button_map = { "3" = "ctrl+left", "4" = "ctrl+right" }  # side buttons: desktop left/right

[mouse.profiles.pointer]
disable_acceleration = true     # 1:1 linear tracking

[[mouse.profiles]]              # optional per-device override profile
device_vendor_id = 1133
device_product_id = 50475
reverse_scroll = false
button_map = { "4" = "cmd+[" }  # side-button actions: "3"/"4" -> [modifier+]key

[[mouse.profiles]]              # optional per-app override profile
app_bundle_id = "com.apple.Safari"
reverse_scroll = false
button_map = { "3" = "cmd+[" }

[[mouse.profiles]]              # per-app continuous scrolling for iPhone Mirroring
app_bundle_id = "com.apple.ScreenContinuity"
scroll_mode = "continuous"

[[mouse.profiles]]              # optional app + device override profile
app_name = "Google Chrome"
device_vendor_id = 1133
device_product_id = 50475
line_count = 5
```

Merge semantics: a profile applies only when both its device matcher and app matcher match.
Specificity decides precedence first, then file order. A global profile has specificity 0, a
device-only or app-only profile has specificity 1, and a profile with both device and app
fields has specificity 2. Higher specificity overrides lower specificity; among profiles with
the same specificity, later profiles win per field. A profile without
`device_vendor_id`/`device_product_id` matches every mouse, and one without
`app_bundle_id`/`app_name` matches every app. `app_bundle_id` matches the running app's bundle
identifier exactly; `app_name` matches its localized name exactly; if both are present, either
may match. Invalid fields (bad `scroll_mode`, `line_count` out of range) are reset to defaults
with a logged error — the app never refuses to start. To find a device's VID:PID, enable debug
logging and watch the `[mouse] scroll dev=` line in the log, or use
`system_profiler SPUSBDataType`.

### Side-button actions (`button_map`)

Each profile can map side buttons to keyboard actions. Button numbers are the HID button
numbers: `3` = back, `4` = forward (only these two are supported in v1). The action syntax
is `[modifier+]key`, case-insensitive:

```toml
button_map = { "3" = "ctrl+left", "4" = "ctrl+right" }   # browser back/forward
button_map = { "4" = "cmd+[" }                            # per-device override
```

Modifiers: `cmd`/`command`, `ctrl`/`control`, `opt`/`option`/`alt`, `shift`. A lone modifier
is valid too, so a side button can be held as a modifier (e.g. `"3" = "cmd"`). Common keys:
letters, digits, and US-layout punctuation (`[`, `]`, `;`, `'`, `,`, `.`, `/`, `-`, `=`,
backtick, `\`), arrows (`left`/`right`/`up`/`down`), `home`/`end`/`page_up`/`page_down`/
`space`/`return`/`tab`/`esc`/`delete`/`forward_delete`, `caps_lock`, `f1`–`f20`, plus
media/system keys `volume_up`, `volume_down`, `mute`, `brightness_up`, `brightness_down`,
`play_pause`, `next_track`, `prev_track`, `mission_control`, `launchpad`. The literal `+`
key is written `plus`.

Semantics: holding the side button presses the target key; releasing it releases the target
(no auto-repeat). A mapped button replaces the original side-button event; unmapped buttons
pass through untouched. An invalid action is dropped with a logged error and never takes down
the rest of the profile.

Logs go to `~/Library/Logs/mouse-control/mouse-control-<timestamp>.log` (30-day retention);
`cargo run` also mirrors them to stdout.

## Menu

- **Enable Mouse Control** — hot start/stop (checkmark shows the state; persisted to config).
- **Reload Config** — re-read the TOML, invalidate the device-resolution cache, re-apply
  pointer settings, and hot start/stop the event tap.
- **Quit** — restores pointer acceleration before exiting.
