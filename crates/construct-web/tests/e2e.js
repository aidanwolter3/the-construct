// The web client in Chromium against a running server (scripts/web_e2e.sh starts one): the code form, a wrong
// code, the right one, home drawn on the canvas, the cookie (HttpOnly, not the pod code), a reload that keeps the
// session, a phone-sized window, and logging out. Screenshots go to the directory given.
//   node crates/construct-web/tests/e2e.js <base url> <pod code> <out dir>
const { chromium } = require('playwright');
const [base, code, out] = process.argv.slice(2);

// How many pixels of a screenshot differ from its corner's colour (a GPU canvas can't be read back, so the
// screenshot is decoded in the page).
async function drawn(page) {
  const png = (await page.screenshot()).toString('base64');
  return page.evaluate(async (b64) => {
    const img = new Image();
    img.src = 'data:image/png;base64,' + b64;
    await img.decode();
    const c = document.createElement('canvas');
    c.width = img.width;
    c.height = img.height;
    const g = c.getContext('2d');
    g.drawImage(img, 0, 0);
    const d = g.getImageData(0, 0, c.width, c.height).data;
    let n = 0;
    for (let i = 0; i < d.length; i += 4 * 7) {
      if (Math.abs(d[i] - d[0]) + Math.abs(d[i + 1] - d[1]) + Math.abs(d[i + 2] - d[2]) > 30) n++;
    }
    return n;
  }, png);
}

function check(ok, what) {
  if (!ok) throw new Error(what);
  console.log('ok   ' + what);
}


(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const errors = [];
  const logs = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => logs.push(m.text()));
  await page.goto(base + '/web');
  await page.waitForSelector('#sign-in:not([hidden])', { timeout: 15000 });
  check(true, 'without a session the page asks for the pod code');
  await page.screenshot({ path: out + '/web-sign-in.png' });

  await page.fill('#code', 'cpod_notacode');
  await page.click('button[type=submit]');
  await page.waitForFunction(() => document.getElementById('code-error').textContent.length > 0);
  check(true, 'a wrong code is refused: ' + (await page.textContent('#code-error')));

  await page.fill('#code', code);
  await page.click('button[type=submit]');
  await page.waitForSelector('body.ready', { timeout: 60000 });
  await page.waitForTimeout(1000);
  check(logs.some((l) => l.startsWith('drawing on the GPU')), 'it draws on the GPU (WebGL2): ' +
    (logs.find((l) => l.startsWith('drawing on') || l.startsWith('no GPU')) || 'no word'));
  await page.screenshot({ path: out + '/web-home-desktop.png' });
  const lit = await drawn(page);
  check(lit > 60, `home is drawn (${lit} sampled pixels differ from the background)`);
  await page.screenshot({ path: out + '/web-home-desktop.png' });

  const cookie = (await page.context().cookies()).find((c) => c.name === 'construct_web');
  check(cookie && cookie.httpOnly && cookie.sameSite === 'Strict', 'the session cookie is HttpOnly and SameSite=Strict');
  check(!cookie.value.includes(code) && !cookie.value.startsWith('cpod_'), 'the cookie is not the pod code');
  check(!(await page.evaluate(() => document.cookie)).includes('construct_web'), 'page scripts cannot read it');

  await page.reload();
  await page.waitForSelector('body.ready', { timeout: 60000 });
  check(true, 'a reload keeps the session');

  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(1000);
  const litPhone = await drawn(page);
  check(litPhone > 30, `a phone-sized window is drawn (${litPhone})`);
  await page.screenshot({ path: out + '/web-home-phone.png' });

  // What Settings' "Leave pod on this device" does in a browser.
  await page.evaluate(() => fetch('/web/logout', { method: 'POST', headers: { 'x-construct-client': 'web' } }));
  await page.reload();
  await page.waitForSelector('#sign-in:not([hidden])', { timeout: 15000 });
  check(true, 'logging out asks for the code again');

  check(errors.length === 0, 'no page errors' + (errors.length ? ': ' + errors.join('; ') : ''));
  await browser.close();
})().catch((e) => {
  console.error('FAIL ' + e.message);
  process.exit(1);
});
