// Runs `js/browser.html` in a REAL browser and reports its verdict.
//
// Node proves the engine works outside Rust; this proves it works on the tier profstopick actually
// ships to. They are different claims: a browser has no `fs`, fetches the index over HTTP, runs the
// query on the main thread next to a render loop, and is the only place `instantiateStreaming` and
// the real MIME-type requirement are exercised.
//
//   cargo build -p index-wasm --release --target wasm32-unknown-unknown
//   cargo run -p index-bench --release --bin emit-artifact
//   node js/browser-check.mjs
//
// It also asserts the FUSED IMAGE QUERY the page runs against the order Node and Rust produce, so
// p53's browser arm is a cross-tier claim and not the page agreeing with itself.
//
// Playwright is NOT a dependency of this repo — it is borrowed from whichever sibling checkout has
// it (presyo and onegrid both do). If `import('playwright')` fails, this exits 2 and says so rather
// than pretending the check ran.

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, extname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const PORT = Number(process.env.INDEX_BROWSER_PORT ?? 8731);

let chromium;
try {
  ({ chromium } = await import('playwright'));
} catch {
  console.error('playwright not resolvable from this repo.');
  console.error('It is deliberately not a dependency; borrow it from a sibling checkout, e.g.');
  console.error('  cmd /c mklink /J node_modules ..\\onegrid\\node_modules');
  process.exit(2);
}

// Served straight out of the tree — no staging step to drift out of date. `artifact/web/` used to
// be the document root, and it silently held an ABI-2 module long after the ABI reached 12: the
// check passed against a build nobody had made that day. The page and the module are now whatever
// the last `cargo build` and the last edit produced, or the request 404s.
const FILE = {
  'index.html': join(root, 'js/browser.html'),
  'index_wasm.wasm': join(root, 'target/wasm32-unknown-unknown/release/index_wasm.wasm'),
  'profstopick.idx': join(root, 'artifact/profstopick.idx'),
  'profstopick.label.json': join(root, 'artifact/profstopick.label.json'),
};

// The image tier's expected ranking, from `js/image-smoke.mjs` run under Node against the same
// module. p53 item 1 is "matches the Rust result exactly — same documents, same order", and this
// is where the two tiers are actually compared: the page reports what it got, and this asserts it.
const NODE_IMAGE = { vector: '1,0,4,2,5,3', fused: '0,2,5' };

const TYPE = {
  '.html': 'text/html; charset=utf-8',
  '.wasm': 'application/wasm', // instantiateStreaming REQUIRES this; text/plain fails the fetch
  '.json': 'application/json',
  '.mjs': 'text/javascript',
};

const server = createServer(async (req, res) => {
  const name = (req.url ?? '/').split('?')[0].replace(/^\/+/, '') || 'index.html';
  const path = FILE[name];
  if (!path) return void res.writeHead(404).end('not found');
  try {
    const body = await readFile(path);
    res.writeHead(200, { 'content-type': TYPE[extname(name)] ?? 'application/octet-stream' });
    res.end(body);
  } catch (err) {
    res.writeHead(500).end(String(err));
  }
});

await new Promise((r) => server.listen(PORT, '127.0.0.1', r));

const browser = await chromium.launch();
const page = await browser.newPage();
const consoleError = [];
page.on('console', (m) => {
  if (m.type() === 'error') consoleError.push(m.text());
});
page.on('pageerror', (e) => consoleError.push(String(e)));

await page.goto(`http://127.0.0.1:${PORT}/index.html`, { waitUntil: 'networkidle' });
await page.waitForFunction(() => document.getElementById('verdict')?.textContent?.length > 0, {
  timeout: 30_000,
});

const meta = (await page.textContent('#meta'))?.replace(/\s+/g, ' ').trim();
const memory = (await page.textContent('#memory'))?.replace(/\s+/g, ' ').trim();
const raw = await page.textContent('#json');
let report = null;
try { report = JSON.parse(raw); } catch { /* the page threw before it could report */ }
const lines = await page.$$eval('#out div', (d) => d.map((x) => x.textContent.trim()));
const verdict = (await page.textContent('#verdict')).trim();
const ua = await page.evaluate(() => navigator.userAgent);

console.log(`browser: ${ua}\n`);
console.log(`  ${meta}\n`);
for (const l of lines) console.log(`  ${l}`);
if (consoleError.length) {
  console.log('\n  console errors:');
  for (const e of consoleError) console.log(`    ${e}`);
}
// The cross-tier assertion. The page checks itself against cosine it computed in the browser; this
// checks the browser against the tier that already passed. Either alone is weaker: p53 item 1 asks
// for the browser result to equal the Rust one, so someone outside the page has to compare them.
let crossFailed = 0;
const cross = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) crossFailed++;
};
console.log('\n  CROSS-TIER (this harness, not the page):');
cross(report?.abi === 12, `the module the browser fetched reports ABI 12 (got ${report?.abi ?? 'nothing'})`);
cross(report?.vector === NODE_IMAGE.vector,
  `browser vector order === Node/Rust ${NODE_IMAGE.vector} (got ${report?.vector ?? 'nothing'})`);
cross(report?.fused === NODE_IMAGE.fused,
  `browser FUSED image order === Node/Rust ${NODE_IMAGE.fused} (got ${report?.fused ?? 'nothing'})`);

console.log(`\n  LINEAR MEMORY, in a browser tab: ${memory}`);
console.log(`  high-water mark ${report?.highWater?.toLocaleString() ?? '?'} B (${report?.page ?? '?'} pages of 64 KiB).`);
console.log("  A tab shares its 4 GiB wasm32 address space with the page's own JS heap, so this is");
console.log('  the number that binds a browser deployment — not latency. `js/image-smoke.mjs`');
console.log('  prints the Node figure and the extrapolated corpus ceiling beside it.');

console.log(`\n${verdict}`);

await browser.close();
server.close();
process.exit(
  verdict.startsWith('OVERALL: PASS') && consoleError.length === 0 && crossFailed === 0 ? 0 : 1,
);
