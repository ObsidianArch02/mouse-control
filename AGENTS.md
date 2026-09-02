# AGENTS.md

This file provides guidance to coding agents working in this repository.

## What this is

`mouse-control` is a minimal macOS menu-bar accessory (no Dock icon) that enhances the
mouse: per-device VID+PID config profiles, reverse/line scroll modes, and optional disabling
of pointer acceleration. It is a stripped-down extract of the mouse-control features from
`oh-my-tab` — there is no window switcher, no settings window, and no keyboard global
shortcut.

It is Rust calling AppKit / CoreGraphics / IOKit directly via `objc2` FFI — no Swift bridge,
no Rust UI framework.

Form factor: a **single unbundled binary** with no `.app` packaging. The binary registers
itself as a launchd LaunchAgent (`mouse-control install`) that auto-starts at login and
restarts on crash; the menu-bar icon still shows because the agent runs in the user's GUI
session.

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

## Install & redeploy as a launchd service

When the user asks to package or redeploy for manual testing, install the binary as a
launchd LaunchAgent via its own subcommand:

```sh
# cargo/rustc come from the mise-managed Rust toolchain (mise activate is wired into the
# shell config; rustup/cargo homes live in ~/.local/share/mise/{rustup,cargo}).
# In a shell without mise activation, prefix with `mise exec --`.
cargo test
cargo build --release

# Copies the binary to ~/.local/bin/mouse-control, ad-hoc signs it, writes
# ~/Library/LaunchAgents/com.eacryo.mouse-control.plist, and bootstraps the agent
# (bootout first when already loaded, so this doubles as the redeploy/restart path).
./target/release/mouse-control install

./target/release/mouse-control status      # install / service / signing state
mouse-control uninstall                    # bootout + remove plist and installed binary
```

**Accessibility permission.** Whenever the app finds itself untrusted, it raises the system
permission prompt: at launch when mouse control is enabled, and again on the menu enable
toggle. The tap thread's ~2-minute retry window means granting takes effect without a
relaunch. TCC matches the grant against the binary's designated requirement, which the
ad-hoc signature pins to the build's cdhash, so **every reinstall of a new build silently
invalidates the grant**: the stale entry still looks enabled under System Settings →
Privacy & Security → Accessibility; remove it and grant again from the prompt. `status`
prints the current designated requirement for diagnosis.

**Permissions & runtime caveats:**
- The HID-level event tap (`defaultTap` option) requires **Accessibility** permission
  (`AXIsProcessTrusted`) to modify/drop events. Without it the tap thread retries (3s × 40)
  and logs; the app keeps running.
- The agent loads only in GUI sessions (`LimitLoadToSessionType=Aqua`,
  `ProcessType=Interactive`), so the menu-bar icon works; it restarts on crash
  (`KeepAlive.SuccessfulExit=false`) but a menu Quit stays off until next login or
  `launchctl kickstart gui/$(id -u)/com.eacryo.mouse-control`.
- Runtime config: `~/.config/mouse-control/config.toml` (auto-created with defaults on first
  run). Pointer settings are restored on quit or when disabled.
- Logs: `~/Library/Logs/mouse-control/mouse-control-<timestamp>.log` (30-day retention);
  launchd's stdout/stderr go to `launchd-{stdout,stderr}.log` in the same directory.
- `assets/Info.plist` is leftover from the `.app` era and is not used by the service form.

## Architecture

Six modules; the load-bearing parts:

- `main.rs` — subcommand dispatch (before any AppKit/CONFIG init), NSApplication accessory
  setup, status-bar icon + menu wiring, startup sequence
  (config load → logger → menu → `mouse::start()` + `mouse::pointer::apply()` when enabled).
- `service.rs` — `install` / `uninstall` / `status` subcommands: copies the binary to
  `~/.local/bin`, ad-hoc signs it, renders the LaunchAgent plist, and drives `launchctl`
  bootstrap/bootout. Never initializes AppKit, so it works from non-GUI sessions.
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
