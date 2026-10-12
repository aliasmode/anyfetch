#!/usr/bin/env bash
# Publishes the npm packages of the latest release for the first time, and tells npm to trust
# the Registry workflow. After that, each release publishes itself, with no token.
#
# npm wants a person to sign in for this. You need an npm account with two-factor
# authentication that owns the name `aliasmode`, as its user name or as an organization.
set -euo pipefail
repo=aliasmode/anyfetch
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { echo "FAILED: $1" >&2; exit 1; }
for tool in gh git node npm tar unzip; do
    command -v "$tool" > /dev/null || fail "the $tool command is missing"
done

# The trust command needs a newer npm than most machines have. The sign-in stays in this
# temporary folder, and is removed at the end.
command npm install --prefix "$work/npm" --no-audit --no-fund --loglevel=error 'npm@^11.15.0'
npm() { NPM_CONFIG_USERCONFIG="$work/npmrc" "$work/npm/node_modules/.bin/npm" "$@"; }

tag=$(gh release view -R "$repo" --json tagName --jq .tagName)
version=${tag#v}
echo "Building the npm packages of $tag."
git clone -q --depth 1 --branch "$tag" "https://github.com/$repo" "$work/source" 2> /dev/null
gh release download "$tag" -R "$repo" -D "$work/release" -p 'anyfetch-*.tar.xz' -p 'anyfetch-*.zip'
"$work/source/npm/build.sh" "$version" "$work/release" "$work/packages"

echo
echo "Sign in to npm: open the link that npm prints below."
echo "When npm asks for two-factor authentication, select the option to skip it for 5 minutes."
npm login --auth-type=web
echo "Signed in as $(npm whoami)."

for folder in "$work"/packages/anyfetch-* "$work/packages/anyfetch"; do
    name=$(node -p "require('$folder/package.json').name")
    npm view "$name@$version" version > /dev/null 2>&1 || npm publish "$folder" --access public
    npm trust github "$name" --file registry.yml --repo "$repo" --allow-publish --yes \
        || npm trust list "$name"
    sleep 2
done
npm logout > /dev/null 2>&1 || true

echo "npm processes a new package for some minutes before it shows it. Waiting."
for _ in $(seq 60); do
    [ "$(npm view @aliasmode/anyfetch version 2> /dev/null)" = "$version" ] && break
    sleep 10
done
[ "$(npm view @aliasmode/anyfetch version 2> /dev/null)" = "$version" ] \
    || fail "npm does not show @aliasmode/anyfetch $version"
echo "SUCCESS: @aliasmode/anyfetch $version is on npm. Try: npx -y @aliasmode/anyfetch --version"
