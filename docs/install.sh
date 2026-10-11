#!/bin/sh
# Installs the latest Anyfetch release. This file only forwards to the release installer.
set -eu
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aliasmode/anyfetch/releases/latest/download/anyfetch-installer.sh | sh
