import { chromium } from 'playwright';
import { mkdirSync } from 'fs';

const url = process.argv[2] || 'http://localhost:8777/index.html';
const out = process.argv[3] || 'shots/shot.png';
const wait = parseInt(process.argv[4] || '6000', 10);
mkdirSync('shots', { recursive: true });

const browser = await chromium.launch({ args: ['--use-gl=angle', '--ignore-gpu-blocklist', '--enable-webgl'] });
const page = await browser.newPage({ viewport: { width: 1680, height: 1020 }, deviceScaleFactor: 2 });
const errors = [];
page.on('console', m => { if (m.type() === 'error') errors.push('CONSOLE: ' + m.text()); });
page.on('pageerror', e => errors.push('PAGEERR: ' + e.message));

const action = process.argv[5] || '';
await page.goto(url, { waitUntil: 'networkidle', timeout: 30000 }).catch(e => errors.push('GOTO: ' + e.message));
await page.waitForTimeout(wait);
if (action === 'select-hub') {
  await page.evaluate(() => window.__ss && window.__ss.select(window.__ss.topHub()));
  await page.waitForTimeout(1600);
} else if (action === 'hover-hub') {
  await page.evaluate(() => window.__ss && window.__ss.hover(window.__ss.topHub()));
  await page.waitForTimeout(900);
}
await page.screenshot({ path: out });

console.log('shot -> ' + out);
if (errors.length) console.log('ERRORS:\n' + errors.join('\n'));
else console.log('no console/page errors');
await browser.close();
