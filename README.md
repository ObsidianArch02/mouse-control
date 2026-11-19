# mouse-control

A macOS menu bar utility for scroll direction, unified scroll travel, and mouse side button remapping.

[Quick Start](#quick-start) | [Service Management](#service-management) | [Configuration](#configuration) | [License](LICENSE)

Requirements: macOS on Apple Silicon (`aarch64-apple-darwin`) and Accessibility permission.

## Features

- Reverse scrolling relative to the macOS scroll direction setting.
- Unified pixel scroll mode (`pixel`) that emits precise pixel events (`line_count * line_height`, default `20` points per line) to bypass per app discrete multipliers.
- Discrete fallback mode (`discrete`) that emits line unit wheel ticks and ignores inherited `line_height`.
- Continuous mode (`continuous`) for trackpad style scrolling in apps such as iPhone Mirroring.
- Side button remapping (`3` and `4`) to keyboard shortcuts or media keys.
- Per device (`device_vendor_id`, `device_product_id`) and per app (`app_bundle_id`, `app_name`) profiles resolved against the window under the cursor.
- In place `CGEvent` mutation on `kCGSessionEventTap` with no pointer acceleration intervention.

## Quick Start

Follow these steps in order to install, sign, and start `mouse-control` as a Homebrew background service in your user GUI session.

### 1. Add the Homebrew tap and install

```sh
brew tap ObsidianArch02/tap
brew install mouse-control
```

### 2. Create a local signing certificate (one-time) and sign

macOS binds Accessibility permission to the binary's code-signing requirement. The binary embeds `assets/Info.plist` (`CFBundleIdentifier = com.eacryo.mouse-control`), so signing with your own local Keychain certificate keeps Accessibility permission intact across future `brew upgrade` runs.

1. Open **Keychain Access**, choose **Certificate Assistant**, then choose **Create a Certificate**.
2. Set **Name** to `mouse-control-codesign`, **Identity Type** to **Self-Signed Root**, and **Certificate Type** to **Code Signing**, then create the certificate.
3. Double-click `mouse-control-codesign` in the **login** keychain, expand **Trust**, and set **Code Signing** to **Always Trust**.
4. Sign the installed binary:

```sh
mouse-control sign
```

Future `brew upgrade mouse-control` runs sign automatically in `post_install` whenever the login keychain is unlocked. You can skip this step to test immediately, but macOS will reset Accessibility permission after each upgrade.

### 3. Start the service and grant Accessibility permission

Start the background service as your normal user (never use `sudo`):

```sh
brew services start mouse-control
```

On first launch, macOS prompts for Accessibility permission under **System Settings -> Privacy & Security -> Accessibility**. Enable **Mouse Control**, then verify the setup:

```sh
mouse-control status
```

A healthy setup reports `instance: running`, `accessibility permission: granted`, and `signature mode: certificate-bound`.

## Service Management

Manage the background lifecycle and upgrades with Homebrew:

```sh
brew services restart mouse-control
brew services stop mouse-control
brew services info mouse-control
brew upgrade mouse-control && brew services restart mouse-control
```

The Homebrew Formula starts `mouse-control run --log-stdout` with `process_type :interactive` and `keep_alive crashed: true`, writing logs to `$(brew --prefix)/var/log/mouse-control.log` and `$(brew --prefix)/var/log/mouse-control.error.log`.

## Configuration

The configuration file follows XDG precedence:

1. `--config <path>`
2. `$XDG_CONFIG_HOME/mouse-control/config.toml`
3. `~/.config/mouse-control/config.toml`

`launchd` does not load interactive shell profiles. When `XDG_CONFIG_HOME` is not exported to `launchd`, the service reads `~/.config/mouse-control/config.toml`.

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

## CLI, Signals, and Menu

Subcommands:

- `mouse-control [run] [--config <path>] [--log-stdout]`: run in the foreground with the menu bar icon.
- `mouse-control status [--config <path>]`: print binary, config, instance lock, Accessibility permission, Homebrew LaunchAgent, and signing state.
- `mouse-control sign [--identity <name>]`: sign the binary with a local Keychain code-signing certificate (default `mouse-control-codesign` or `MOUSE_CONTROL_CODESIGN_IDENTITY`).
- `mouse-control --version`: print the version.

Runtime signals and menu actions:

- `SIGTERM` and `SIGINT`: release held side buttons, stop the event tap, and exit cleanly.
- `SIGHUP`: reload the configuration file and clear resolution caches.
- Enable Mouse Control: toggle the event tap at runtime and persist `mouse.enabled`.
- Reload Config: reload the active configuration file and clear resolution caches.
- Quit: release held side button keys, stop the event tap, and exit.

## Build from Source

```sh
cargo test --locked
cargo build --release --locked
./target/release/mouse-control run
```

## License

See [LICENSE](LICENSE).
