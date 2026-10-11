#!/bin/sh
# Starts the Anyfetch MCP server for the Claude Code plugin. If the program is not on this
# machine yet, the release installer puts it in ~/.local/bin first. Standard output belongs to
# the MCP protocol, so the installer writes to standard error.
set -eu
if command -v anyfetch >/dev/null 2>&1; then
    exec anyfetch mcp
fi
if [ ! -x "$HOME/.local/bin/anyfetch" ]; then
    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aliasmode/anyfetch/releases/latest/download/anyfetch-installer.sh | sh >&2
fi
exec "$HOME/.local/bin/anyfetch" mcp
