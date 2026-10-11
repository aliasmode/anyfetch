#!/bin/sh
# Installs the latest Anyfetch release, then connects it to the agent harnesses on this machine.
set -eu
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aliasmode/anyfetch/releases/latest/download/anyfetch-installer.sh | sh
if [ -x "$HOME/.local/bin/anyfetch" ]; then
    "$HOME/.local/bin/anyfetch" setup || true
fi
