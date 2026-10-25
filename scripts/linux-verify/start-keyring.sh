#!/usr/bin/env bash
set -euo pipefail

eval "$(printf 'linux-verify-password\n' | gnome-keyring-daemon --daemonize --login --components=secrets)"

# Headless GNOME Keyring only exposes an unlocked session collection, so use it as the default without a GUI prompt.
gdbus call --session --dest org.freedesktop.secrets --object-path /org/freedesktop/secrets \
  --method org.freedesktop.Secret.Service.SetAlias default /org/freedesktop/secrets/collection/session >/dev/null
