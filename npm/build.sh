#!/bin/sh
# Builds the npm packages of a release from its archives: one package per platform that holds
# the program, and the main package that starts the one for the user's platform.
#   npm/build.sh <version> <folder with the release archives> <output folder>
set -eu
version=$1 archives=$2 out=$3
here=$(dirname "$0")

package() { # <npm os> <npm cpu> <Rust target> <program file>
    folder="$out/anyfetch-$1-$2"
    mkdir -p "$folder"
    case $3 in
        *windows*) unzip -q -o -j "$archives/anyfetch-$3.zip" "$4" -d "$folder" ;;
        *) tar -xJf "$archives/anyfetch-$3.tar.xz" -C "$folder" --strip-components=1 "anyfetch-$3/$4" ;;
    esac
    # The Linux programs need glibc, so npm must not pick them on a musl system.
    libc=''
    [ "$1" = linux ] && libc='"libc": ["glibc"],'
    cat > "$folder/package.json" <<JSON
{
  "name": "@aliasmode/anyfetch-$1-$2",
  "version": "$version",
  "description": "The Anyfetch program for $1 $2. Install @aliasmode/anyfetch, which selects it.",
  "license": "Apache-2.0",
  "repository": { "type": "git", "url": "git+https://github.com/aliasmode/anyfetch.git" },
  "os": ["$1"],
  "cpu": ["$2"],
  $libc
  "files": ["$4"]
}
JSON
}

package linux x64 x86_64-unknown-linux-gnu anyfetch
package linux arm64 aarch64-unknown-linux-gnu anyfetch
package darwin x64 x86_64-apple-darwin anyfetch
package darwin arm64 aarch64-apple-darwin anyfetch
package win32 x64 x86_64-pc-windows-msvc anyfetch.exe

mkdir -p "$out/anyfetch"
cp "$here/run.js" "$here/../README.md" "$here/../LICENSE" "$out/anyfetch/"
sed "s/0\.0\.0/$version/g" "$here/package.json" > "$out/anyfetch/package.json"
