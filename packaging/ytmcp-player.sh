#!/bin/sh
# Launcher for the packaged YTMcp runtime under /opt/ytmcp.
# The host handles --background --no-activate for the local controller.
res=/opt/ytmcp/resources
export YTMCP_DAEMON_BIN="$res/bin/ytm-daemon"
export YTMCP_UI_DIST_DIR="$res/ui-dist"
export YTMCP_BRIDGE_DIST_DIR="$res/bridge-dist"
export YTMCP_IDENTITY_PATH="$res/config/app-identity.json"
exec /opt/ytmcp/ytmcp-player "$@"
