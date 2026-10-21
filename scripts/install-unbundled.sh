#!/bin/bash
# Run oh-my-tab without an .app bundle: build the release binary, install it as a plain
# executable, and keep it running through a per-user launchd agent.
#
# For machines where .app bundles cannot be launched (for example managed Macs whose endpoint
# policy only blocks unapproved bundles), or for users who prefer a CLI-managed install.
#
# The app already degrades without a bundle: launch at login is provided by the launchd agent
# instead of SMAppService, Sparkle updates are unavailable (re-run `install` after pulling), and
# the bundle version strings are empty. Only explicitly named arguments are read; nothing from
# the caller's environment is forwarded to the app (launchd starts it with its own environment).
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install-unbundled.sh <command> [options]

Commands:
  install     Build (cargo build --release), install the binary, register and start the
              launchd agent, and install the launcher (unless --no-launcher)
  start       Start the agent (loads it first when it is not loaded)
  stop        Stop the agent until the next login or `start`
  restart     Stop, then start
  status      Show the agent state and the last log lines
  launcher    (Re)install only the launcher
  uninstall   Stop the agent and remove the binary, the agent plist and the launcher
              (configuration and logs are kept)

Options:
  --identity <name|sha1>  Code-signing identity. Default: the "Developer ID Application"
                          identity when exactly one is installed, else "oh-my-tab-sign" when
                          present, else ad-hoc (macOS then asks for permissions again after
                          every rebuild; see docs/releasing-en.md for oh-my-tab-sign)
  --bin-dir <dir>         Where the binary is installed (default: ~/.local/bin)
  --no-launcher           Do not install the launcher
  -h, --help              Show this help

The launcher, "Oh My Tab.command", goes to /Applications (or ~/Applications when /Applications
is not writable). It is a shell script, not an app: double-click it in Finder or open it from
Spotlight to start oh-my-tab again after quitting it from the menu.
EOF
}

REPO="$(cd "$(dirname "$0")/.." && pwd)"
NAME="oh-my-tab"
LABEL="com.eacryo.oh-my-tab.unbundled"
# A signing identifier distinct from the .app's bundle id, so TCC grants of an installed app
# and of this binary never refer to the same identifier with different code requirements.
SIGN_IDENTIFIER="com.eacryo.oh-my-tab.unbundled"
DOMAIN="gui/$(id -u)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/Library/Logs/oh-my-tab"
APP_LOG="$LOG_DIR/oh-my-tab.log"
LAUNCHD_LOG="$LOG_DIR/launchd.log"
LAUNCHER_NAME="Oh My Tab.command"

BIN_DIR="$HOME/.local/bin"
IDENTITY=""
WANT_LAUNCHER=1

COMMAND="${1:-}"
[[ -n "$COMMAND" ]] && shift
while [[ $# -gt 0 ]]; do
    case "$1" in
        --identity) IDENTITY="${2:?--identity needs a value}"; shift 2 ;;
        --identity=*) IDENTITY="${1#*=}"; shift ;;
        --bin-dir) BIN_DIR="${2:?--bin-dir needs a value}"; shift 2 ;;
        --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
        --no-launcher) WANT_LAUNCHER=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done
# launchd needs an absolute path in ProgramArguments.
[[ "$BIN_DIR" == /* ]] || BIN_DIR="$PWD/$BIN_DIR"
BIN="$BIN_DIR/$NAME"

resolve_identity() {
    [[ -n "$IDENTITY" ]] && { echo "$IDENTITY"; return; }
    local developer_ids count
    developer_ids="$(/usr/bin/security find-identity -v -p codesigning 2>/dev/null \
        | /usr/bin/awk -F'"' '/"Developer ID Application:/ { print $2 }' || true)"
    count="$(printf '%s' "$developer_ids" | /usr/bin/grep -c . || true)"
    if [[ "$count" == 1 ]]; then
        echo "$developer_ids"
    # Without -v: the self-signed certificate created as documented is not trusted, so -v hides
    # it, yet codesign signs with it (as bundle.sh does). Captured first: under pipefail, an early
    # `grep -q` exit can SIGPIPE `security` and turn a match into a failure.
    elif /usr/bin/grep -q '"oh-my-tab-sign"' <<<"$(/usr/bin/security find-identity -p codesigning 2>/dev/null || true)"; then
        echo "oh-my-tab-sign"
    else
        echo "-"
    fi
}

build() {
    (cd "$REPO" && cargo build --release)
}

install_binary() {
    local identity
    identity="$(resolve_identity)"
    mkdir -p "$BIN_DIR"
    # Installed outside target/, which the logger treats as a production build (file logging).
    cp "$REPO/target/release/$NAME" "$BIN.new"
    /usr/bin/codesign --force --sign "$identity" --identifier "$SIGN_IDENTIFIER" "$BIN.new"
    mv -f "$BIN.new" "$BIN"
    echo "Installed $BIN"
    if [[ "$identity" == "-" ]]; then
        echo "warning: signed ad-hoc. macOS will ask for Accessibility / Screen Recording (and the"
        echo "         clipboard keychain item) again after every rebuild. Create the self-signed"
        echo "         'oh-my-tab-sign' identity (docs/releasing-en.md) to keep grants across rebuilds."
    else
        echo "Signed with: $identity"
    fi
}

write_plist() {
    mkdir -p "$(dirname "$PLIST")" "$LOG_DIR"
    # RunAtLoad starts it at login. KeepAlive restarts it only after an abnormal exit, so quitting
    # from the menu (exit 0) keeps it quit; a second instance also exits 0 when the instance lock
    # is held (e.g. by an installed .app), so the two never fight.
    cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$LABEL</string>
    <key>ProgramArguments</key><array><string>$BIN</string><string>--no-onboarding</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
    <key>ProcessType</key><string>Interactive</string>
    <key>LimitLoadToSessionType</key><string>Aqua</string>
    <key>StandardOutPath</key><string>$LAUNCHD_LOG</string>
    <key>StandardErrorPath</key><string>$LAUNCHD_LOG</string>
</dict>
</plist>
EOF
}

is_loaded() { launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; }

stop_agent() {
    launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
    # bootout returns before the job is gone; bootstrapping too early fails with
    # "Bootstrap failed: 5: Input/output error".
    local _
    for _ in $(seq 1 50); do
        is_loaded || return 0
        sleep 0.1
    done
    echo "warning: $LABEL is still loaded after 5s" >&2
}

start_agent() {
    [[ -f "$PLIST" ]] || { echo "error: $PLIST not found; run: $0 install" >&2; exit 1; }
    if is_loaded; then
        # Loaded but not running (quit from the menu): bootstrap would fail, kickstart starts it.
        launchctl kickstart "$DOMAIN/$LABEL"
    else
        launchctl bootstrap "$DOMAIN" "$PLIST"
    fi
}

status() {
    if is_loaded; then
        launchctl print "$DOMAIN/$LABEL" \
            | awk '/^\tstate =/{s=$3} /^\tpid =/{p=$3} END{print "agent: " s (p ? " (pid " p ")" : "")}'
    else
        echo "agent: not loaded"
    fi
    [[ -f "$APP_LOG" ]] && tail -n 3 "$APP_LOG" | sed 's/^/  /'
    return 0
}

launcher_path() {
    if [[ -w /Applications ]]; then
        echo "/Applications/$LAUNCHER_NAME"
    else
        echo "$HOME/Applications/$LAUNCHER_NAME"
    fi
}

install_launcher() {
    local target
    target="$(launcher_path)"
    mkdir -p "$(dirname "$target")"
    sed "s|@LABEL@|$LABEL|g" "$REPO/scripts/unbundled-launcher.command" > "$target"
    chmod 755 "$target"
    # Give the script the app icon and hide its extension so Finder shows "Oh My Tab". The icon
    # is stored in the file's resource fork, so it has to be applied after every copy.
    /usr/bin/osascript -l JavaScript - "$REPO/assets/Icon-512x512.png" "$target" >/dev/null <<'JXA'
ObjC.import('AppKit');
function run(argv) {
    const image = $.NSImage.alloc.initWithContentsOfFile(argv[0]);
    $.NSWorkspace.sharedWorkspace.setIconForFileOptions(image, argv[1], 0);
    $.NSFileManager.defaultManager.setAttributesOfItemAtPathError(
        $({ NSFileExtensionHidden: true }), argv[1], null);
}
JXA
    echo "Installed launcher: $target"
}

remove_launcher() {
    rm -f "/Applications/$LAUNCHER_NAME" "$HOME/Applications/$LAUNCHER_NAME"
}

case "$COMMAND" in
    install)
        build
        stop_agent
        install_binary
        write_plist
        start_agent
        [[ "$WANT_LAUNCHER" == 1 ]] && install_launcher
        sleep 2
        status
        echo
        echo "Grant Accessibility (required) and Screen & System Audio Recording (thumbnails) to"
        echo "$BIN under System Settings > Privacy & Security, then run: $0 restart" ;;
    start) start_agent; sleep 2; status ;;
    stop) stop_agent; echo "Stopped until the next login or: $0 start" ;;
    restart) stop_agent; start_agent; sleep 2; status ;;
    status) status ;;
    launcher) install_launcher ;;
    uninstall)
        stop_agent
        # Remove the binary the agent points at, wherever --bin-dir put it at install time.
        if [[ -f "$PLIST" ]]; then
            installed_bin="$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:0' "$PLIST" 2>/dev/null || true)"
            [[ -n "$installed_bin" ]] && BIN="$installed_bin"
        fi
        rm -f "$PLIST" "$BIN"
        remove_launcher
        echo "Uninstalled. ~/.config/oh-my-tab and $LOG_DIR are kept; remove $NAME from"
        echo "Privacy & Security > Accessibility and Screen & System Audio Recording." ;;
    -h|--help) usage ;;
    *) usage >&2; exit 2 ;;
esac
