// Renders pages in a Chromium browser through Playwright, for anyfetch (src/playwright.rs).
// Requests come on stdin and answers go to stdout, one JSON object per line:
//   in  {"id": 1, "url": "https://..."}
//   out {"id": 1, "url": "...", "status": 200, "html": "..."} or {"id": 1, "error": "..."}
// The first line out is {"ready": true} or {"fatal": "..."}. anyfetch picks the browser and passes
// it in ANYFETCH_PW_* variables. The helper exits when stdin closes
// or after IDLE_MS without a request, so a browser it started does not outlive anyfetch.
import { existsSync } from "node:fs";
import { delimiter, join } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const IDLE_MS = 5 * 60 * 1000;
const PAGE_MS = 30 * 1000;
const SETTLE_MS = 8 * 1000;

/** The Playwright package: the one anyfetch found, or the one npx/bunx put on PATH. */
function packageDir() {
  if (process.env.ANYFETCH_PW_MODULE) return process.env.ANYFETCH_PW_MODULE;
  const bins = (process.env.PATH ?? "").split(delimiter).filter((dir) => /node_modules[\\/]\.bin$/.test(dir));
  for (const bin of bins) {
    for (const name of ["playwright-core", "playwright"]) {
      const dir = join(bin, "..", name);
      if (existsSync(join(dir, "index.js"))) return dir;
    }
  }

  return undefined;
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

async function open(chromium) {
  const cdp = process.env.ANYFETCH_PW_CDP;
  if (cdp) {
    const browser = await chromium.connectOverCDP(cdp, { timeout: PAGE_MS });
    return browser.contexts()[0] ?? (await browser.newContext());
  }

  // launchOptions shared with the Playwright MCP server; its args add to these.
  const shared = JSON.parse(process.env.ANYFETCH_PW_LAUNCH ?? "{}");
  const args   = ["--no-first-run", "--no-default-browser-check", ...(shared.args ?? [])];
  const named  = process.env.ANYFETCH_PW_PROFILE_DIR;
  if (named && !args.some((arg) => arg.startsWith("--profile-directory="))) args.push(`--profile-directory=${named}`);
  return chromium.launchPersistentContext(process.env.ANYFETCH_PW_PROFILE, {
    headless: true,
    timeout:  PAGE_MS,
    ...shared,
    executablePath: process.env.ANYFETCH_PW_EXE,
    args,
  });
}

async function render(context, url) {
  const page = await context.newPage();
  try {
    const response = await page.goto(url, { waitUntil: "domcontentloaded", timeout: PAGE_MS });
    // Pages that keep a connection open never go idle; what loaded by then is what they show.
    await page.waitForLoadState("networkidle", { timeout: SETTLE_MS }).catch(() => {});
    return { url: page.url(), status: response?.status() ?? 200, html: await page.content() };
  } finally {
    await page.close().catch(() => {});
  }
}

const dir = packageDir();
if (!dir) {
  send({ fatal: "Playwright was not found: no ANYFETCH_PW_MODULE and no playwright-core on PATH" });
  process.exit(1);
}

let context;
try {
  const playwright = await import(pathToFileURL(join(dir, "index.js")).href);
  context = await open(playwright.chromium ?? playwright.default.chromium);
} catch (error) {
  send({ fatal: String(error?.message ?? error).split("\n")[0] });
  process.exit(1);
}

let pending = 0;
let closed  = false;
let idle    = setTimeout(() => process.exit(0), IDLE_MS);
send({ ready: true });
const lines = createInterface({ input: process.stdin });
lines.on("line", (line) => {
  clearTimeout(idle);
  idle = setTimeout(() => process.exit(0), IDLE_MS);
  const { id, url } = JSON.parse(line);
  pending++;
  render(context, url)
    .then(
      (page) => send({ id, ...page }),
      (error) => send({ id, error: String(error?.message ?? error).split("\n")[0] }),
    )
    .finally(() => {
      pending--;
      if (closed && pending === 0) process.exit(0);
    });
});
lines.on("close", () => {
  closed = true;
  if (pending === 0) process.exit(0);
});
