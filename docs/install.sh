#!/bin/sh
# Installs the latest Fetchroam release. This file only forwards to the release installer.
set -eu
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aliasmode/fetchroam/releases/latest/download/fetchroam-installer.sh | sh
