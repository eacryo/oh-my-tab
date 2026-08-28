#!/bin/bash
# Run the development app under launchd, then remove the submitted job when the
# app exits. launchctl submit infers keepalive, so this prevents a normal menu
# quit from being interpreted as a request to restart the app.

launch_label="$1"
shift

"$@"
exit_code=$?

launch_domain="gui/$(id -u)"
launchctl bootout "$launch_domain/$launch_label" >/dev/null 2>&1 || true
exit "$exit_code"
