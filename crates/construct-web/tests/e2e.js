// The web client in Chromium against a running server (scripts/web_e2e.sh starts one): the code form, a wrong
// code, the right one, home drawn on the canvas, the cookie (HttpOnly, not the pod code), a reload that keeps the
// session, a phone-sized window, and logging out. Screenshots go to the directory given.
//   node crates/construct-web/tests/e2e.js <base url> <pod code> <out dir>
const { chromium } = require('playwright');
const [base, code, out] = process.argv.slice(2);

function check(ok, what) {
  if (!ok) throw new Error(what);
  console.log('ok   ' + what);
}

// Whether the canvas drew anything (a pixel that isn't the first one's colour).
const drawn = (page) => page.evaluate(() => {
  const c = document.getElementById('construct');
  const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
  for (let i = 4; i < d.length; i += 4 * 97) {
    if (d[i] !== d[0] || d[i + 1] !== d[1] || d[i + 2] !== d[2]) return true;
  }
  return false;
});

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
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
  check(await drawn(page), 'home is drawn on the canvas');
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
  check(await drawn(page), 'a phone-sized window is drawn');
  await page.screenshot({ path: out + '/web-home-phone.png' });

  await page.click('#log-out');
  await page.waitForSelector('#sign-in:not([hidden])', { timeout: 15000 });
  check(true, 'logging out asks for the code again');

  check(errors.length === 0, 'no page errors' + (errors.length ? ': ' + errors.join('; ') : ''));
  await browser.close();
})().catch((e) => {
  console.error('FAIL ' + e.message);
  process.exit(1);
});
