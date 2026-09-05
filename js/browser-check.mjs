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
// Playwright is NOT a dependency of this repo — it is borrowed from whichever sibling checkout has
// it (presyo and onegrid both do). If `import('playwright')` fails, this exits 2 and says so rather
// than pretending the check ran.

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, extname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const web = join(root, 'artifact', 'web');
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

const TYPE = {
  '.html': 'text/html; charset=utf-8',
  '.wasm': 'application/wasm', // instantiateStreaming REQUIRES this; text/plain fails the fetch
  '.json': 'application/json',
  '.mjs': 'text/javascript',
};

const server = createServer(async (req, res) => {
  const name = (req.url ?? '/').split('?')[0].replace(/^\/+/, '') || 'index.html';
  try {
    const body = await readFile(join(web, name));
    res.writeHead(200, { 'content-type': TYPE[extname(name)] ?? 'application/octet-stream' });
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

await page.goto(`http://127.0.0.1:${PORT}/index.html`, { waitUntil: 'networkidle' });
await page.waitForFunction(() => document.getElementById('verdict')?.textContent?.length > 0, {
  timeout: 30_000,
});

const meta = (await page.textContent('#meta'))?.replace(/\s+/g, ' ').trim();
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
console.log(`\n${verdict}`);

await browser.close();
server.close();
process.exit(verdict.startsWith('OVERALL: PASS') && consoleError.length === 0 ? 0 : 1);
