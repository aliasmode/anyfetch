# Installs the latest Anyfetch release, then connects it to the agent harnesses on this machine.
irm https://github.com/aliasmode/anyfetch/releases/latest/download/anyfetch-installer.ps1 | iex
$anyfetch = Join-Path $HOME ".local\bin\anyfetch.exe"
if (Test-Path $anyfetch) { & $anyfetch setup }
