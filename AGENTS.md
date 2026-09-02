# AGENTS.md

This file provides guidance to coding agents working in this repository.

## What this is

`mouse-control` is a minimal macOS menu-bar accessory app (no Dock icon) that enhances the
mouse: per-device VID+PID config profiles, reverse/line scroll modes, and optional disabling
of pointer acceleration. It is a stripped-down extract of the mouse-control features from
`oh-my-tab` — there is no window switcher, no settings window, and no keyboard global
shortcut.

It is Rust calling AppKit / CoreGraphics / IOKit directly via `objc2` FFI — no Swift bridge,
no Rust UI framework.

## Build & run

```sh
cargo build      # or cargo check for fast type-checking
cargo test       # unit tests in mouse/resolve.rs and mouse/scrolling.rs
cargo run        # build + run (takes over the mouse event tap)
cargo clippy     # available, not wired into CI
```

`cargo`/`rustc` come from the mise-managed Rust toolchain (declared in `programs.mise` in
`~/nix-darwin/home.nix`). In interactive shells mise activate provides them; in
non-interactive shells use `mise exec -- cargo ...`.

## Packaging & redeploying to /Applications

When the user asks to package or redeploy for manual testing, deploy the app to
`/Applications/Mouse Control.app` (not `~/Desktop` or `dist/`). Reuse the bundle layout from
the first packaging session: release binary + `assets/Info.plist` + `assets/statusbar-icon.png`,
then ad-hoc sign and verify.

On every redeploy, reset the Accessibility permission before opening the app. That removes
the old TCC grant and makes the next launch trigger the system permission prompt again:

```sh
# cargo/rustc come from the mise-managed Rust toolchain (mise activate is wired into the
# shell config; rustup/cargo homes live in ~/.local/share/mise/{rustup,cargo}).
# In a shell without mise activation, prefix with `mise exec --`.
cargo test
cargo build --release

MOUSE_APP_DIR="/Applications/Mouse Control.app"
MOUSE_BUNDLE_ID="com.eacryo.mouse-control"

# Stop the current instance before replacing the bundle.
pkill -x mouse-control 2>/dev/null || true

# Remove the old Accessibility grant so the next launch re-prompts.
# If assets/Info.plist's CFBundleIdentifier ever changes, update MOUSE_BUNDLE_ID.
tccutil reset Accessibility "$MOUSE_BUNDLE_ID"

# Rebuild the bundle in /Applications.
rm -rf "$MOUSE_APP_DIR"
mkdir -p "$MOUSE_APP_DIR/Contents/MacOS" "$MOUSE_APP_DIR/Contents/Resources"
cp target/release/mouse-control "$MOUSE_APP_DIR/Contents/MacOS/mouse-control"
cp assets/Info.plist "$MOUSE_APP_DIR/Contents/Info.plist"
cp assets/statusbar-icon.png "$MOUSE_APP_DIR/Contents/Resources/statusbar-icon.png"

# Ad-hoc local signing is enough for local testing.
# Use Developer ID + notarization only if the user asks for distribution.
codesign --force --deep --sign - "$MOUSE_APP_DIR"
codesign --verify --deep --strict --verbose=2 "$MOUSE_APP_DIR"

open "$MOUSE_APP_DIR"
```

After `open`, do not silently skip the permission step: the user should see the macOS
Accessibility prompt, or open System Settings → Privacy & Security → Accessibility and enable
**Mouse Control**. Logs remain at `~/Library/Logs/mouse-control/mouse-control-<timestamp>.log`.

**Permissions & runtime caveats:**
- The HID-level event tap (`defaultTap` option) requires **Accessibility** permission
  (`AXIsProcessTrusted`) to modify/drop events. Grant it under System Settings → Privacy &
  Security → Accessibility. Without it the tap thread retries (3s × 40) and logs; the app
  keeps running. A freshly built binary at a new path must be re-granted.
- Runtime config: `~/.config/mouse-control/config.toml` (auto-created with defaults on first
  run). Pointer settings are restored on quit or when disabled.
- Logs: `~/Library/Logs/mouse-control/mouse-control-<timestamp>.log` (30-day retention).

## Architecture

Six modules; the load-bearing parts:

- `main.rs` — NSApplication accessory setup, status-bar icon + menu wiring, startup sequence
  (config load → logger → menu → `mouse::start()` + `mouse::pointer::apply()` when enabled).
- `menu.rs` — the three menu items (Enable Mouse Control toggle / Reload Config / Quit) and
  their callbacks. The toggle persists `mouse.enabled` and hot start/stops the event tap;
  reload invalidates the resolve cache and re-applies pointer settings; quit restores them.
- `mouse.rs` — module entry; idempotent `start()`/`stop()` for the mouse event thread.
- `mouse/event_tap.rs` — the mouse event tap on a dedicated thread (HID level, `defaultTap`).
  Scroll events are attributed to a device, resolved to an effective profile, then
  transformed (reverse / line mode) via synthetic events posted to the session layer; the
  original event is dropped. Trackpad (continuous) scroll is passed through untouched.
- `mouse/device.rs` — live device registry + event→device attribution chain
  (`CGEventCopyIOHIDEvent` → `IOHIDEventGetSenderID` → registry lookup) and the IOHIDManager
  plug/unplug monitor (Bluetooth reconnect recovery). All state is on the mouse thread.
- `mouse/pointer.rs` — disables pointer acceleration per device (macOS 14+ linear-scaling
  key; legacy `HIDPointerAcceleration` fallback), saving originals and restoring them on
  disable/quit.
- `mouse/resolve.rs` — merges the "All Mice" profile + per-device profiles (later wins) into
  a concrete `ResolvedMouse`, with a cache invalidated on reload.
- `mouse/scrolling.rs` — scroll-mode semantics (Default passthrough+reverse / Line fixed
  count) and delta computation.
- `mouse/ffi.rs` — all CoreGraphics/CoreFoundation/IOKit extern declarations, event-tap
  semantic constants, the synthetic-event marker, and `create_tap_with_retry`.
- `config.rs` — TOML config (`[mouse]`, `[logging]`, `[i18n]`), per-field resilient
  load/validate/merge/save, hot reload, global `CONFIG` singleton.
- `ffi.rs` — shared CF/ObjC primitives (`make_nsstring`, `CFRelease`, `CFEqual`,
  `has_accessibility_permission`, objc class registration).
- `logger.rs` — two-tier logging (debug/info) to a per-launch file with 30-day retention.
- `i18n.rs` — handcrafted TOML i18n (en/zh-Hans/zh-Hant) with automatic system-language
  follow. **Never let i18n read `CONFIG`** — `CONFIG`'s LazyLock init calls
  `i18n::apply_config_locale`, and a read-back would deadlock. Only config.rs calls
  `apply_config_locale`, one-way.

## Event flow & threading

1. The mouse event tap runs on a dedicated thread with its own `CFRunLoop`; the scroll
   transformation must happen synchronously inside the tap callback (no channel hop — the
   event is already being consumed).
2. The main thread owns `NSApplication` and the status-bar menu; menu callbacks start/stop
   the mouse thread and re-apply settings.
3. Shared state is `static`s guarded by `Mutex`/`OnceLock`/`LazyLock` (`CONFIG`,
   `MOUSE_THREAD`, device `REGISTRY`, `POINTER_STATE`, resolve `CACHE`, `MOUSE_ENABLE_ITEM`).
   Raw ObjC/CF pointers are kept alive explicitly (`retain`, services arrays, or wrapper
   structs with manual `Send`/`Sync`).

## Conventions

- **Commit only on `codex/*` branches.** The agent may stage and commit when the current
  branch name starts with `codex/`; on any other branch, only *generate* the commit-message
  text (Conventional Commits, single line) and leave the git writes to the user.
- **Never push.** Pushing to a remote stays with the user.
- Comments at non-obvious logic must be bilingual, Chinese first then English, mirroring the
  existing style.
- All user-visible strings go through `t()` / `tf()` and exist in all three locale files
  (`en`, `zh-Hans`, `zh-Hant`). Log lines stay English (developer-facing).
- Some AppKit/CoreFoundation calls that touch CF/CG types or return void must use raw
  `objc_msgSend` FFI — `objc2`'s `msg_send!` can't encode those. See the existing patterns in
  `mouse/ffi.rs` and `ffi.rs`.
