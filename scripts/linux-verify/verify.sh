#!/usr/bin/env bash
set -euo pipefail

exec dbus-run-session -- bash -euc '
  source /workspace/scripts/linux-verify/start-keyring.sh
  exec bash /workspace/scripts/linux-verify/verify-in-session.sh
'
