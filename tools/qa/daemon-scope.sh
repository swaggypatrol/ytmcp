#!/bin/sh
# QA only: runs the real ytm-daemon in its own transient user scope so its egress can be
# cut separately from the Electron processes (G4 failure isolation). stdio is inherited.
exec systemd-run --user --scope --quiet --unit="ytm-glass-qa-daemon-$$" \
  --description="YTM Glass QA daemon scope" -- "${YTM_GLASS_QA_REAL_DAEMON:?}" "$@"
