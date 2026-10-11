#!/usr/bin/env node
// Starts Anyfetch. npm installs the package that holds the program for this platform; this
// file finds it and passes the arguments on.
const { spawnSync } = require('node:child_process');

const platform = `${process.platform}-${process.arch}`;
const file = process.platform === 'win32' ? 'anyfetch.exe' : 'anyfetch';
let program;
try {
  program = require.resolve(`@aliasmode/anyfetch-${platform}/${file}`);
} catch {
  console.error(`Anyfetch has no npm package for ${platform}. See https://github.com/aliasmode/anyfetch#install`);
  process.exit(1);
}
const result = spawnSync(program, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`Anyfetch could not start: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
