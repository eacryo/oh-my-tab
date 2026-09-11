# Development Notes

These notes cover issues that affect source builds (`cargo run` / debuggers), not users of Homebrew installations or packaged `.app` builds. The README links here for development-only issues.

## Icons may be incorrect in development mode

When running the bare binary with `cargo run`, the overlay may occasionally show oh-my-tab's own card as an initial-letter placeholder instead of its application icon, and the problem may persist until the icon cache is cleared manually. The icon cache is keyed by bundle ID and uses the executable **mtime** as its invalidation fingerprint. Each development build relinks the binary and changes its mtime, invalidating the running instance's cache entry. Packaged `.app` builds are unaffected because the installed binary's mtime remains stable. During development, use the *Clear Icon Cache* menu item or delete `~/Library/Caches/oh-my-tab-icons/`.

## Mouse control may fail when launched from a debugger

When the app is launched in Debug mode through RustRover or another debugger, frequent mouse activity (scrolling/clicking) during startup may cause mouse control features such as scroll reversal and per-device settings to stop working. The app no longer receives mouse events, scrolling returns to the system default, and pointer-acceleration settings stop taking effect until the app is restarted. Launching a packaged `.app` directly or running the binary from a terminal is unaffected. This only occurs with unsigned development builds launched by a debugger, due to macOS 26 restrictions on HID-layer event monitoring for debugger processes.

## Logging and memory diagnostics

The default log path is `~/Library/Logs/oh-my-tab/oh-my-tab.log`. Once the active file reaches 10 MB, it rolls through `oh-my-tab.log.1` to `oh-my-tab.log.5`. Each launch writes a session marker; legacy per-launch logs and backups older than 30 days are cleaned up at startup.

After roughly 60 seconds, the app writes its first `[mem]` sample, followed by one every 5 minutes. The log includes the active feature profile, process footprint/RSS, sampled footprint peak, thread count, and estimated thumbnail, clipboard, and window ledgers. `footprint` is macOS's physical-footprint metric and corresponds to the Activity Monitor Memory column; it is the primary number for assessing memory pressure. `rss` is current resident memory and may fall as macOS compresses or reclaims pages. `footprint_peak_sampled` is sampled by the app, while `rss_peak_kernel` is the kernel's process-lifetime high-water mark.

The clipboard ledger separates text, preview, and metadata estimates; original image bytes in the disk cache are not counted as resident memory. Logs contain no clipboard contents or window imagery. Debug logs record only `Tab`, `Command`, `Option`, and the summon-combination name from the switcher's key tap. All other keys are logged as `Other`, without keycodes or modifier details.

## Device identification details

To determine whether a device is a mouse or trackpad, the app checks whether it conforms to Generic Desktop Pointer (1,1), Mouse (1,2), or Trackpad (1,5) usages. It uses the public `IOHIDServiceClientConformsTo` API against the complete `DeviceUsagePairs`, rather than relying on a single `PrimaryUsage` value. This matters because some real mice report an incorrect primary usage: for example, **ATK A9 SE** (a Nearlink mouse) reports `PrimaryUsage = 6 (Keyboard)` and appears as a keyboard in System Settings, while its `DeviceUsagePairs` also declares Mouse (1,2). `ConformsTo` identifies it correctly. Looking only at `PrimaryUsage` would silently discard such a device and apply its events to the “recently used” profile instead.

Bluetooth keyboards are excluded even when their HID descriptors incorrectly advertise pointer usages (for example, Kzzi-i75 declares a complete Mouse collection). The device picker cross-checks the Bluetooth **GAP Appearance** value (`0x03C1` = keyboard), using the cache written by `bluetoothd` to NVRAM and matching the HID service's Bluetooth address. This is the same source used for the macOS Bluetooth panel icon. Devices absent from the NVRAM cache, such as newly paired devices, and non-Bluetooth devices fall back to the HID-only check. The device picker refreshes in real time: unplugged devices disappear immediately and reconnected devices reappear automatically. Plug/unplug events are debounced without being dropped, and delayed rechecks cover fast BLE sleep/wake cycles.

## Building and testing

`scripts/dev-restart.sh` is the normal way to run the app during development. It builds and assembles a separately signed development `.app` and launches it through the per-user `launchd` domain, so Accessibility and Screen Recording permissions stay associated with the development bundle and the running process always contains the latest build.

The unit-test suite is headless-safe by default; clipboard image and history fixtures use isolated temporary directories per process and thread. The CG/AX **smoke tests** are marked `#[ignore]` and need a GUI session plus an Accessibility grant:

```sh
cargo test -- --ignored
```

On a macOS GUI session, the real AppKit settings smoke test can traverse every settings page and run the post-layout checks without manual clicking:

```sh
cargo build
cargo test settings_layout_smoke -- --ignored
```

It runs the debug binary with `--smoke-settings-layout`, opens the actual settings window on the main thread, visits all seven pages, validates their descendant view frames, and exits.

### Localization and layout QA

The Debug app built by `scripts/dev-restart.sh` adds a `[TEST] English x3` option to the language selector. Selecting it repeats every English UI string three times, so long dropdown values and their surrounding rows and cards can be checked in the real settings window. The optimized development package built by `scripts/release-dev.sh` includes the same fixture through the `dev-long-text` Cargo feature; the production release scripts do not enable it. The older `OH_MY_TAB_PSEUDO_LOCALE=1` switch is still available for debug-only punctuation-based expansion.

Set `OH_MY_TAB_LAYOUT_DEBUG=1` in a debug build to enable runtime settings-page assertions; overlapping controls, out-of-bounds frames, and separators with an invalid layer order fail fast with the page name and offending frames. These checks complement visual review instead of requiring it for every layout change.
