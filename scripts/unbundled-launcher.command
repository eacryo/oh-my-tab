#!/bin/bash
# "Oh My Tab" launcher installed by scripts/install-unbundled.sh: double-click in Finder or open
# from Spotlight to start the unbundled oh-my-tab. It is not an .app; it only starts the launchd
# agent, posts a notification, and closes its own Terminal window.

LABEL="@LABEL@"
DOMAIN="gui/$(id -u)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
MY_TTY="$(tty 2>/dev/null)"

notify() {
    /usr/bin/osascript -e "display notification \"$1\" with title \"Oh My Tab\"" >/dev/null 2>&1
}

close_window() {
    [[ "$TERM_PROGRAM" == "Apple_Terminal" && "$MY_TTY" == /dev/* ]] || return 0
    # Close the window only when this script's tab is its only tab, so a launcher opened as a tab
    # never takes the user's other tabs with it. The closer runs in a new session (setsid, no
    # controlling tty): left on this tab's tty, Terminal would ask whether to terminate it while it
    # waits for that very sheet, a deadlock.
    local script="tell application \"Terminal\"
    repeat with w in (every window whose tty of selected tab is \"$MY_TTY\")
        if (count of tabs of w) is 1 then close w
    end repeat
end tell"
    /usr/bin/perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' \
        /bin/bash -c 'sleep 0.5; /usr/bin/osascript -e "$1"' _ "$script" </dev/null >/dev/null 2>&1 &
}

is_running() { launchctl print "$DOMAIN/$LABEL" 2>/dev/null | grep -q $'^\tstate = running'; }

if [[ ! -f "$PLIST" ]]; then
    echo "oh-my-tab is not installed: $PLIST is missing."
    echo "Install it with scripts/install-unbundled.sh install in the oh-my-tab repository."
    notify "Not installed"
    exit 1
fi

if is_running; then
    notify "Already running"
else
    if launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
        launchctl kickstart "$DOMAIN/$LABEL"
    else
        launchctl bootstrap "$DOMAIN" "$PLIST"
    fi
    for _ in $(seq 1 30); do
        is_running && break
        sleep 0.1
    done
    if ! is_running; then
        echo "Failed to start $LABEL; see ~/Library/Logs/oh-my-tab/launchd.log"
        notify "Failed to start"
        exit 1
    fi
    notify "Started"
fi

close_window
exit 0
