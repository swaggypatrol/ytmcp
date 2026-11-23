#!/bin/sh
# QA only: runs the real ytm-daemon in its own transient user scope so its egress can be
# cut separately from the Electron processes (G4 failure isolation). stdio is inherited.
exec systemd-run --user --scope --quiet --unit="ytmcp-qa-daemon-$$" \
  --description="YTMcp QA daemon scope" -- "${YTMCP_QA_REAL_DAEMON:?}" "$@"
