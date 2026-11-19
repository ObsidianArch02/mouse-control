# mouse-control

A macOS menu bar utility for scroll direction, unified scroll travel, and mouse side button remapping.

[Install](#install) | [Configuration](#configuration) | [License](LICENSE)

Requirements: macOS on Apple Silicon (`aarch64-apple-darwin`) and Accessibility permission.

## Features

- Reverse scrolling relative to the macOS scroll direction setting.
- Unified pixel scroll mode (`pixel`) that emits precise pixel events (`line_count * line_height`, default `20` points per line) to bypass per app discrete multipliers.
- Discrete fallback mode (`discrete`) that emits line unit wheel ticks and ignores inherited `line_height`.
- Continuous mode (`continuous`) for trackpad style scrolling in apps such as iPhone Mirroring.
- Side button remapping (`3` and `4`) to keyboard shortcuts or media keys.
- Per device (`device_vendor_id`, `device_product_id`) and per app (`app_bundle_id`, `app_name`) profiles resolved against the window under the cursor.
- In place `CGEvent` mutation on `kCGSessionEventTap` with no pointer acceleration intervention.

## Install

Build from source and install as a launchd LaunchAgent:

```sh
cargo test --locked
cargo build --release --locked
./target/release/mouse-control install
```

Subcommands:

- `mouse-control [run]`: run in the foreground with the menu bar icon.
- `mouse-control install`: copy the binary to `~/.local/bin/mouse-control`, ad hoc sign it, and load the LaunchAgent.
- `mouse-control status`: print install, service, and signing state.
- `mouse-control uninstall`: unload the agent and remove the plist and installed binary.

Grant Accessibility permission under System Settings, Privacy & Security, Accessibility. After installing a new build, remove the previous `mouse-control` entry before granting permission again.

## Configuration

The configuration file lives at `~/.config/mouse-control/config.toml`:

```toml
[logging]
level = "info"
file_path = ""

[i18n]
locale = "auto"

[mouse]
enabled = true

[[mouse.profiles]]
reverse_scroll = true
scroll_mode = "pixel"
line_count = 3

[mouse.profiles.button_map]
3 = "ctrl+right"
4 = "ctrl+left"

[[mouse.profiles]]
app_bundle_id = "com.apple.ScreenContinuity"
scroll_mode = "continuous"

[[mouse.profiles]]
app_bundle_id = "com.mitchellh.ghostty"
scroll_mode = "pixel"
line_count = 1

[[mouse.profiles]]
app_bundle_id = "com.apple.Notes"
line_count = 2
line_height = 24
```

Scroll modes:

- `pixel`: emits `kCGScrollEventUnitPixel` (`line_count * line_height`, default `line_height = 20`).
- `discrete`: emits `kCGScrollEventUnitLine` (`line_count` ticks, ignoring inherited `line_height`).
- `continuous`: emits continuous pixel deltas from the raw event with optional reversal.
- `line`: legacy mode (`kCGScrollEventUnitPixel` when `line_height` is set, `kCGScrollEventUnitLine` when absent).
- `default`: passes raw wheel deltas through with optional reversal.

Profile merge precedence uses specificity first (global, then device only or app only, then device plus app) and file order within the same tier.

## Menu

- Enable Mouse Control: toggle the event tap at runtime and persist `mouse.enabled`.
- Reload Config: reload `~/.config/mouse-control/config.toml` and clear resolution caches.
- Quit: release held side button keys and exit.

## License

See [LICENSE](LICENSE).
