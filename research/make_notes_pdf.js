// Render docs/notes/notes.html to docs/notes/RuseTect study notes.pdf (A4 portrait).
// Usage: node research/make_notes_pdf.js   (after python3 -I research/build_notes.py)
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const root = path.resolve(__dirname, '..');
  const b = await chromium.launch();
  const p = await b.newPage();
  await p.goto('file://' + root + '/docs/notes/notes.html', { waitUntil: 'load' });
  await p.pdf({ path: path.join(root, 'docs/notes/RuseTect study notes.pdf'), format: 'A4', printBackground: true,
    margin: { top: '14mm', bottom: '14mm', left: '14mm', right: '14mm' } });
  await b.close();
})();
