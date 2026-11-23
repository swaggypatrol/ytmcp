#!/bin/sh
# Launcher for the packaged YTM Glass runtime under /opt/ytm-glass.
# The host handles --background --no-activate for the local controller.
res=/opt/ytm-glass/resources
export YTM_GLASS_DAEMON_BIN="$res/bin/ytm-daemon"
export YTM_GLASS_UI_DIST_DIR="$res/ui-dist"
export YTM_GLASS_BRIDGE_DIST_DIR="$res/bridge-dist"
export YTM_GLASS_IDENTITY_PATH="$res/config/app-identity.json"
exec /opt/ytm-glass/ytm-glass "$@"
