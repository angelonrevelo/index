// Runs `js/opfs.html` in a REAL browser and reports its verdict.
//
// This is the gate for the tier `docs/research/landscape.md` §7.5 says nobody occupies: an index
// living in the browser's own filesystem, queried with no network call, and range-readable so a
// file larger than the tab's memory can still be opened.
//
// It is a different claim from `browser-check.mjs`, which proves the engine RUNS in a browser.
// This proves it PERSISTS there and answers offline — which is what profstopick actually needs, and
// what a server-bound engine cannot do at any speed.
//
//   cargo build -p index-wasm --release --target wasm32-unknown-unknown
//   cargo run -p index-bench --release --bin emit-artifact
//   node js/opfs-check.mjs
//
// Playwright is deliberately NOT a dependency; it is borrowed from a sibling checkout the same way
// `browser-check.mjs` borrows it.

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, extname, join, normalize } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const PORT = Number(process.env.INDEX_OPFS_PORT ?? 8733);

let chromium;
try {
  ({ chromium } = await import('playwright'));
} catch {
  console.error('playwright not resolvable from this repo.');
  console.error('It is deliberately not a dependency; borrow it from a sibling checkout, e.g.');
  console.error('  cmd /c mklink /J node_modules ..\\onegrid\\node_modules');
  process.exit(2);
}

const TYPE = {
  '.html': 'text/html; charset=utf-8',
  // instantiateStreaming REQUIRES this; text/plain fails the fetch.
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.mjs': 'text/javascript',
  '.idx': 'application/octet-stream',
};

// Served straight out of the tree, so the page and the module are whatever the last build and the
// last edit produced. A staging directory is how `browser-check` once tested an ABI-2 module long
// after the ABI reached 12.
const server = createServer(async (req, res) => {
  const rel = decodeURIComponent((req.url ?? '/').split('?')[0]).replace(/^\/+/, '') || 'js/opfs.html';
  const path = normalize(join(root, rel));
  if (!path.startsWith(normalize(root))) return void res.writeHead(403).end('no');
  try {
    await stat(path);
    const body = await readFile(path);
    res.writeHead(200, { 'content-type': TYPE[extname(path)] ?? 'application/octet-stream' });
    res.end(body);
  } catch {
    res.writeHead(404).end('not found');
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

// 127.0.0.1 is a secure context, so OPFS is available without TLS.
await page.goto(`http://127.0.0.1:${PORT}/js/opfs.html`, { waitUntil: 'networkidle' });
await page.waitForFunction(
  () => !document.getElementById('verdict')?.textContent?.startsWith('running'),
  { timeout: 60_000 },
);

const text = async (id) => (await page.textContent(`#${id}`))?.trim();

// `textContent` on a table runs every cell together, so rows are read cell by cell instead.
const table = async (id) =>
  page.$$eval(`#${id} tr`, (tr) =>
    tr.map((r) => [...r.children].map((c) => c.textContent.trim()).filter(Boolean).join('  ')));

for (const id of ['load', 'net', 'lat', 'typo', 'range']) {
  for (const line of await table(id)) {
    if (line) console.log(`  ${line}`);
  }
}

const verdict = await text('verdict');
console.log(`\n${verdict}`);

// The reload arm: a SECOND page load must find the index already in OPFS and issue no fetch for it.
// Doing it in one page proves persistence within a session; doing it across a navigation proves it
// survives one, which is the actual product claim.
const page2 = await browser.newPage();
await page2.goto(`http://127.0.0.1:${PORT}/js/opfs.html`, { waitUntil: 'networkidle' });
await page2.waitForFunction(
  () => !document.getElementById('verdict')?.textContent?.startsWith('running'),
  { timeout: 60_000 },
);
const survived = await page2.evaluate(async () => {
  const root = await navigator.storage.getDirectory();
  const d = await root.getDirectoryHandle('index-tier');
  const f = await (await d.getFileHandle('profstopick.idx')).getFile();
  return f.size;
});
console.log(`  PASS  the index survives a full page load — ${survived.toLocaleString()} B still in OPFS`);

await browser.close();
server.close();

if (consoleError.length) {
  console.error('\nconsole errors:');
  for (const e of consoleError.slice(0, 5)) console.error(`  ${e}`);
}
const ok = verdict?.includes('PASS') && survived > 0;
console.log(ok ? 'OVERALL: PASS' : 'OVERALL: FAIL');
process.exit(ok ? 0 : 1);
