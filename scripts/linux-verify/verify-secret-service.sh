#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."

dbus-run-session -- bash -euc '
  source scripts/linux-verify/start-keyring.sh
  cargo run -p yc-ai --example linux_secret_service_probe -- save
  cargo run -p yc-ai --example linux_secret_service_probe -- verify
  cargo run -p yc-ai --example linux_secret_service_probe -- remove
'

env -u DBUS_SESSION_BUS_ADDRESS cargo run -p yc-ai --example linux_secret_service_probe -- unavailable

env -u DBUS_SESSION_BUS_ADDRESS \
  YAKUCRAFT_E2E_USE_SYSTEM_CREDENTIAL_STORE=1 \
  YAKUCRAFT_E2E_SPEC="$(pwd)/e2e/specs/linux-secret-service-unavailable.spec.mjs" \
  xvfb-run -a npm run test:e2e
