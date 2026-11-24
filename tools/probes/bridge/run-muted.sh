#!/usr/bin/env bash
# Runs a command with the default sink muted and restores the previous mute state on any exit.
set -u
SINK=$(pactl get-default-sink)
WAS=$(LC_ALL=C pactl get-sink-mute "$SINK" | grep -q "yes" && echo 1 || echo 0)
restore() { pactl set-sink-mute "$SINK" "$WAS"; }
trap restore EXIT INT TERM HUP
pactl set-sink-mute "$SINK" 1
"$@"
