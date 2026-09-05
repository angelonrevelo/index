// Runs `js/geo-browser.html` in a REAL browser and reports its verdict.
//
// Node proves the WASM point-location engine works outside Rust; this proves it works on the tier a
// map actually ships to. They are different claims: a browser has no `fs`, fetches the module over
// HTTP, runs the query on the main thread next to a render loop, and is the only place
// `instantiateStreaming` and the real `application/wasm` MIME requirement are exercised.
//
//   cargo build -p index-geo-wasm --release --target wasm32-unknown-unknown
//   node js/geo-browser-check.mjs
//
// Playwright is NOT a dependency of this repo — it is borrowed from whichever sibling checkout has
// it (presyo, onegrid and maphy all do). If `import('playwright')` fails, this exits 2 and says so
// rather than pretending the check ran.

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, extname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const PORT = Number(process.env.INDEX_BROWSER_PORT ?? 8741);

let chromium;
try {
  ({ chromium } = await import('playwright'));
} catch {
  console.error('playwright not resolvable from this repo.');
  console.error('It is deliberately not a dependency; borrow it from a sibling checkout, e.g.');
  console.error('  cmd /c mklink /J node_modules ..\\onegrid\\node_modules');
  process.exit(2);
}

// Served straight out of the tree — no staging step to drift out of date.
const FILE = {
  'index.html': join(root, 'js/geo-browser.html'),
  'geo.mjs': join(root, 'js/geo.mjs'),
  'index_geo_wasm.wasm': join(root, 'target/wasm32-unknown-unknown/release/index_geo_wasm.wasm'),
  'maphy-poi.csv': join(root, 'bench/fixture/maphy-poi.csv'),
  'maphy-municipal.txt': join(root, 'bench/fixture/maphy-municipal.txt'),
};

const TYPE = {
  '.html': 'text/html; charset=utf-8',
  '.wasm': 'application/wasm', // instantiateStreaming REQUIRES this; octet-stream fails the fetch
  '.mjs': 'text/javascript; charset=utf-8',
  '.csv': 'text/csv; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
};

const server = createServer(async (req, res) => {
  const name = (req.url ?? '/').split('?')[0].replace(/^\/+/, '') || 'index.html';
  const path = FILE[name];
  if (!path) return void res.writeHead(404).end('not found');
  try {
    const body = await readFile(path);
    res.writeHead(200, { 'content-type': TYPE[extname(name)] ?? 'application/octet-stream' });
    res.end(body);
  } catch (e) {
    res.writeHead(500).end(String(e));
  }
});

await new Promise((r) => server.listen(PORT, '127.0.0.1', r));

const browser = await chromium.launch();
const page = await browser.newPage();
const consoleError = [];
page.on('console', (m) => { if (m.type() === 'error') consoleError.push(m.text()); });
page.on('pageerror', (e) => consoleError.push(String(e)));

let verdict;
try {
  await page.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: 'load' });
  await page.waitForFunction('window.__geo && (window.__geo.ok || window.__geo.error)', null,
    { timeout: 120_000 });
  verdict = await page.evaluate('window.__geo');
} finally {
  await browser.close();
  server.close();
}

console.log(`browser: ${verdict.ua}`);
for (const r of verdict.row) {
  console.log(`  ${r.arm.padEnd(28)}${(r.ms.toFixed(2) + 'ms').padStart(11)}`
    + `${((r.rate / 1e6).toFixed(2) + ' M pts/s').padStart(16)}`
    + `${(r.ratio.toFixed(1) + 'x').padStart(9)}`);
}
if (consoleError.length) {
  console.error('console errors:');
  for (const e of consoleError) console.error('  ' + e);
}
if (!verdict.ok) {
  console.error('FAILED: ' + verdict.error);
  process.exit(1);
}
console.log('geo browser check: ok');
