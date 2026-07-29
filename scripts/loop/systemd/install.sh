#!/usr/bin/env bash
# Install the tick timer as a user service.
#
# Separate from the units themselves because installing them is a change to the
# machine, not to the repository, and it needs one privileged step: without
# lingering, a user service stops the moment the ssh session ends - which would
# make the fleet run only while someone is watching, the exact opposite of the
# point.
set -euo pipefail

UNITS="$(cd "$(dirname "$0")" && pwd)"
DEST="$HOME/.config/systemd/user"

[ -f "$HOME/.config/flowproof-loop.env" ] \
  || { echo "no ~/.config/flowproof-loop.env; the fleet would have no credentials" >&2; exit 1; }

mkdir -p "$DEST"
install -m 644 "$UNITS/flowproof-loop.service" "$UNITS/flowproof-loop.timer" "$DEST/"
systemctl --user daemon-reload
systemctl --user enable --now flowproof-loop.timer

if [ "$(loginctl show-user "$USER" --property=Linger --value 2>/dev/null)" != "yes" ]; then
  echo
  echo "Lingering is off, so the timer will stop when this session ends."
  echo "Enable it with:"
  echo
  echo "  sudo loginctl enable-linger $USER"
  echo
fi

systemctl --user list-timers flowproof-loop.timer --no-pager
