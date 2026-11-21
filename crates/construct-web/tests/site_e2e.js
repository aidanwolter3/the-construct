// The web client on the owner's own site (decision `site-origins`) in Chromium, against a running server whose
// CONSTRUCT_SITE_ORIGINS lists http://127.0.0.1:<site port> (scripts/web_e2e.sh starts one). An agent shares a list
// of meals as @sitecook/meals; this test serves a made-up static site on that port, with the strict policy a real
// site sets, and checks:
// - /construct/ asks for a sign-in; the pod code is typed on the server's page, not the site's; back on the site the
//   pod draws, and a reload stays signed in; Log out ends the session;
// - /meals/ draws the public link @sitecook/meals, a tap pushes a recipe with its own address on the site, and that
//   address (served by the site's 404 page, as GitHub Pages does) opens it;
// - another site's page can't trade a code or read the server's answers.
//   node crates/construct-web/tests/site_e2e.js <server url> <pod code> <mcp key> <site port> <out dir>
const http = require('http');
const { chromium } = require('playwright');
const [server, code, key, sitePort, out] = process.argv.slice(2);
const site = `http://127.0.0.1:${sitePort}`;

function check(ok, what) {
  if (!ok) throw new Error(what);
  console.log('ok   ' + what);
}

async function pixel(page, x, y) {
  const png = (await page.screenshot()).toString('base64');
  return page.evaluate(async ([b64, x, y]) => {
    const img = new Image();
    img.src = 'data:image/png;base64,' + b64;
    await img.decode();
    const c = document.createElement('canvas');
    c.width = img.width;
    c.height = img.height;
    const g = c.getContext('2d');
    g.drawImage(img, 0, 0);
    const d = g.getImageData(Math.round(x * img.width / innerWidth), Math.round(y * img.height / innerHeight), 1, 1).data;
    return [d[0], d[1], d[2]];
  }, [png, x, y]);
}
const green = ([r, g, b]) => r < 60 && g > 70 && g < 120 && b < 60;

async function tool(name, args) {
  const r = await fetch(server + '/v1/mcp', {
    method: 'POST',
    headers: { authorization: 'Bearer ' + key, 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }),
  });
  const v = await r.json();
  const text = v.result && v.result.content && v.result.content[0].text;
  return text ? JSON.parse(text) : v;
}

// A list of meals whose rows push their recipe (a green screen the test can see), shared at /sitecook/meals.
async function shareMeals() {
  await tool('define_collection', { name: 'site_meals', fields: [{ name: 'name', type: 'text' }] });
  for (const name of ['Lentil soup', 'Shakshuka']) await tool('add_row', { collection: 'site_meals', values: { name } });
  const feeds = [['collections', 'site_meals']];
  const recipe = await tool('create_layout', { title: 'Site recipe', feeds, layout: { type: 'core://Page',
    id: 'site_recipe', title: 'Recipe', root: { type: 'core://Box', fill: true, background: '#1b5e20',
      content: { type: 'core://Text', style: 'Headline', value_expr: '${collections://site_meals/$route.id#name}' } } } });
  const meals = await tool('create_layout', { title: 'Site meals', feeds, layout: { type: 'core://Page',
    id: 'site_meals', title: 'Meals', root: { type: 'core://Column', children: [
      { type: 'core://Box', height_dp: 64, padding_h_dp: 16, content: { type: 'core://Text', style: 'Headline', value_expr: 'Meals' } },
      { type: 'core://List', query_key: 'collection:site_meals', item_template: { type: 'core://Tappable',
        on_click: { intent: 'action.navigation.open', ref: recipe.id, params: { id: '$item.id' }, presentation: 'Push' },
        content: { type: 'core://Box', height_dp: 64, padding_h_dp: 16,
          content: { type: 'core://Text', style: 'Title', value_expr: '$item.name' } } } }] } } });
  await tool('set_handle', { handle: 'sitecook' });
  const review = await tool('share_publicly', { layout: meals.id, slug: 'meals' });
  const link = await tool('share_publicly', { layout: meals.id, slug: 'meals', confirm: review.review_id });
  check(link.short_url && link.short_url.endsWith('/sitecook/meals'), 'an agent shared ' + link.short_url);
}

// The site's pages, as the owner's static site writes them.
const csp = `default-src 'none'; script-src 'self' ${server} 'wasm-unsafe-eval'; connect-src ${server}; ` +
  `style-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'`;
const pageOf = (mode, extra = '') => `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Made-up site</title><link rel="stylesheet" href="/site.css"></head>
<body><div id="construct-root" data-mode="${mode}" ${extra}></div>
<script type="module" src="${server}/web/mount.js"></script></body></html>`;
const files = {
  '/construct/': pageOf('app'),
  '/meals/': pageOf('public', 'data-handle="sitecook"'),
  '/site.css': 'html,body{margin:0;height:100%;overflow:hidden}canvas#construct{display:block;touch-action:none}' +
    '#construct-input{position:fixed;left:-1000px;top:0;opacity:0;width:1px;height:1px}',
  '/other.html': '<!doctype html><title>Other</title>',
};
const notFound = pageOf('public', 'data-handle="sitecook"');

function serve(port) {
  return new Promise((done) => {
    const s = http.createServer((req, res) => {
      const path = new URL(req.url, site).pathname;
      const body = files[path];
      const type = path.endsWith('.css') ? 'text/css' : 'text/html; charset=utf-8';
      res.writeHead(body ? 200 : 404, { 'content-type': type });
      res.end(body || notFound);
    });
    s.listen(port, '127.0.0.1', () => done(s));
  });
}

(async () => {
  await shareMeals();
  const siteServer = await serve(Number(sitePort));
  // A second, unlisted origin: the server must not answer it.
  const other = await serve(0);
  const otherOrigin = `http://127.0.0.1:${other.address().port}`;

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  // A 401 (the session check once logged out) and the site's 404 page (a recipe's address) are expected.
  page.on('console', (m) => {
    if (m.type() === 'error' && !/status of (401|404)/.test(m.text())) errors.push(m.text());
  });

  await page.goto(site + '/construct/');
  await page.waitForSelector('#construct-sign-in:not([hidden])', { timeout: 30000 });
  check(true, 'the site\'s /construct/ asks for a sign-in');
  await page.click('#construct-sign-in');
  await page.waitForURL(server + '/web/site?**');
  check(page.url().startsWith(server + '/web/site'), 'the pod code is asked for on the server\'s own page');
  check((await page.textContent('body')).includes('127.0.0.1:' + sitePort), 'which names the site');
  await page.fill('input[name=pod_code]', code);
  await page.click('button');
  await page.waitForURL(site + '/construct/**');
  await page.waitForSelector('body.ready', { timeout: 60000 });
  await page.waitForTimeout(1200);
  check(page.url() === site + '/construct/', 'back on the site, the code is gone from the address: ' + page.url());
  const token = await page.evaluate(() => localStorage.getItem('construct.site.session'));
  check(token && !token.startsWith('cpod_'), 'the site keeps a browser session, not the pod code');
  check(await page.isHidden('#construct-sign-in'), 'and the pod draws');
  await page.screenshot({ path: out + '/site-pod.png' });

  await page.reload();
  await page.waitForSelector('body.ready', { timeout: 60000 });
  check(await page.isHidden('#construct-sign-in'), 'a reload stays signed in');

  // The session is a bearer the server takes from the site only as a browser's (no MCP).
  const mcp = await fetch(server + '/v1/mcp', { method: 'POST', headers: { authorization: 'Bearer ' + token,
    'content-type': 'application/json', accept: 'application/json' }, body: '{}' });
  check(mcp.status === 403, 'the site\'s session doesn\'t reach the MCP endpoint');

  // Another origin's page: no CORS answer, so it can't read the server or trade a code.
  const otherPage = await context.newPage();
  await otherPage.goto(otherOrigin + '/other.html');
  const blocked = await otherPage.evaluate(async ([server, token]) => {
    try {
      await fetch(server + '/v1/auth/sign-ins', { headers: { authorization: 'Bearer ' + token } });
      return false;
    } catch {
      return true;
    }
  }, [server, token]);
  check(blocked, 'an unlisted site can\'t read the server, even with the token');
  await otherPage.close();

  const out1 = await page.evaluate(async ([server, token]) => {
    const r = await fetch(server + '/web/logout', { method: 'POST',
      headers: { 'x-construct-client': 'web', authorization: 'Bearer ' + token } });
    return r.status;
  }, [server, token]);
  check(out1 === 200, 'Log out from the site');
  await page.reload();
  await page.waitForSelector('#construct-sign-in:not([hidden])', { timeout: 30000 });
  check(true, 'and the session has ended: the site asks again');

  // A public link at the site's own address.
  await page.goto(site + '/meals/');
  await page.waitForSelector('body.ready', { timeout: 60000 });
  await page.waitForTimeout(1000);
  check(!green(await pixel(page, 195, 600)), 'the site\'s /meals/ draws @sitecook/meals');
  await page.mouse.click(195, 96);
  await page.waitForTimeout(1200);
  check(green(await pixel(page, 195, 600)), 'a tap pushes the recipe from the copy');
  check(page.url() === site + '/meals/lentil-soup', 'with its own address on the site: ' + page.url());
  await page.screenshot({ path: out + '/site-recipe.png' });
  await page.goto(site + '/meals/shakshuka');
  await page.waitForSelector('body.ready', { timeout: 60000 });
  await page.waitForTimeout(1200);
  check(green(await pixel(page, 195, 600)), 'a recipe\'s address on the site (its 404 page) opens it');

  check(errors.length === 0, 'no page errors' + (errors.length ? ': ' + errors.join('; ') : ''));
  await browser.close();
  siteServer.close();
  other.close();
})().catch((e) => {
  console.error('FAIL ' + e.message);
  process.exit(1);
});
