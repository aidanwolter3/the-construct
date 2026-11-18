// A public link in Chromium (decision `public-web-links`) against a running server with CONSTRUCT_PUBLIC_WEB=1
// (scripts/web_e2e.sh starts one): an agent makes a made-up recipes collection and layout over MCP, reviews and
// shares it; a visitor with no pod opens the page; a new row reaches it; a field the page doesn't show never does;
// unsharing stops it. Then (decision `public-links-share-the-closure`) a list whose rows push their recipe is shared
// with a short path: a visitor taps a row, the recipe shows, and Back returns. Last, a link opens in a new tab as
// desktop Chrome opens one (running the page's frame inside window.open) and the page still answers taps after.
// Screenshots go to the directory given.
//   node crates/construct-web/tests/public_e2e.js <base url> <mcp key> <out dir>
const { chromium } = require('playwright');
const [base, key, out] = process.argv.slice(2);

// The colour of the page at (x, y), from a screenshot decoded in the page (a GPU canvas can't be read back).
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

function check(ok, what) {
  if (!ok) throw new Error(what);
  console.log('ok   ' + what);
}

async function tool(name, args) {
  const r = await fetch(base + '/v1/mcp', {
    method: 'POST',
    headers: { authorization: 'Bearer ' + key, 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }),
  });
  const v = await r.json();
  const text = v.result && v.result.content && v.result.content[0].text;
  return text ? JSON.parse(text) : v;
}

(async () => {
  await tool('define_collection', { name: 'recipes', fields: [
    { name: 'title', type: 'text' }, { name: 'minutes', type: 'number' }, { name: 'notes', type: 'text', optional: true }] });
  for (const [title, minutes] of [['Lentil soup', 40], ['Shakshuka', 30], ['Green curry', 35]]) {
    await tool('add_row', { collection: 'recipes', values: { title, minutes, notes: 'only for me' } });
  }
  const made = await tool('create_layout', { title: 'Recipes', feeds: [['collections', 'recipes']], layout: {
    type: 'core://Page', id: 'recipes', title: 'Recipes', root: { type: 'core://Scroll', content: {
      type: 'core://Column', max_width_dp: 960, padding_h_dp: 16, padding_v_dp: 16, spacing_dp: 12, children: [
        { type: 'core://Text', style: 'Headline', value_expr: 'Recipes' },
        { type: 'core://List', query_key: 'collection:recipes', min_column_dp: 240, spacing_dp: 12, item_template: {
          type: 'core://Box', corner_dp: 14, padding_h_dp: 14, padding_v_dp: 12, border_color: '${local://theme#card_border}',
          content: { type: 'core://Column', children: [
            { type: 'core://Text', style: 'Label', value_expr: '$item.title' },
            { type: 'core://Text', style: 'Secondary', value_expr: '$item.minutes min' }] } } }] } } } });
  check(made.id, 'an agent made a recipes layout');
  await tool('update_layout', { id: 'home', layout: { type: 'core://Page', id: 'home', title: 'Home',
    root: { type: 'core://Layout', ref: made.id } } });
  const review = await tool('share_publicly', { layout: made.id });
  check(review.review_id && !review.anyone_with_the_link_reads.fields.includes('notes'),
    'the review lists what goes, without the field the page doesn\'t show');
  const shared = await tool('share_publicly', { layout: made.id, confirm: review.review_id });
  check(shared.link && shared.url, 'confirmed: ' + shared.url);

  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(base + '/s/' + shared.link);
  await page.waitForSelector('body.ready', { timeout: 60000 });
  await page.waitForTimeout(800);
  check(await page.isHidden('#sign-in'), 'a visitor needs no pod and no code');
  check((await page.title()) === 'Recipes', 'the page has the copy\'s title');
  await page.screenshot({ path: out + '/public-desktop.png' });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(800);
  await page.screenshot({ path: out + '/public-phone.png' });

  await tool('add_row', { collection: 'recipes', values: { title: 'Plum cake', minutes: 70, notes: 'only for me' } });
  let fresh = false;
  for (let i = 0; i < 20 && !fresh; i++) {
    await page.waitForTimeout(500);
    fresh = (await (await page.request.get(base + '/s/' + shared.link + '/view')).text()).includes('Plum cake');
  }
  check(fresh, 'a new row reaches the page');
  const view = await (await page.request.get(base + '/s/' + shared.link + '/view')).text();
  check(!view.includes('only for me'), 'the field the page doesn\'t show never leaves the pod');

  await tool('unshare_publicly', { link: shared.link });
  check((await page.request.get(base + '/s/' + shared.link)).status() === 404, 'unsharing stops the page at once');

  // A list whose rows push their recipe, shared whole at a short path.
  await tool('define_collection', { name: 'meals', fields: [{ name: 'name', type: 'text' }] });
  await tool('define_collection', { name: 'lines', fields: [{ name: 'meal_id', type: 'text' }, { name: 'thing', type: 'text' }] });
  for (const [meal, things] of [['Lentil soup', ['Red lentils', 'Cumin']], ['Shakshuka', ['Eggs', 'Tomatoes']]]) {
    const row = await tool('add_row', { collection: 'meals', values: { name: meal } });
    for (const thing of things) await tool('add_row', { collection: 'lines', values: { meal_id: row.id, thing } });
  }
  const feeds = [['collections', 'meals'], ['collections', 'lines']];
  // The recipe fills the screen with a green the test can see.
  const recipe = await tool('create_layout', { title: 'Recipe', feeds, layout: { type: 'core://Page', id: 'recipe',
    title: 'Recipe', root: { type: 'core://Box', fill: true, background: '#1b5e20', content: { type: 'core://Column',
      padding_h_dp: 16, padding_v_dp: 16, spacing_dp: 8, children: [
        { type: 'core://Text', style: 'Headline', value_expr: '${collections://meals/$route.id#name}' },
        { type: 'core://List', query_key: 'collection:lines', where: [{ field: 'meal_id', equals: '$route.id' }],
          item_template: { type: 'core://Text', style: 'Body', value_expr: '$item.thing' } }] } } } });
  check(recipe.id, 'an agent made a recipe screen that reads $route.id');
  const meals = await tool('create_layout', { title: 'Meals', feeds, layout: { type: 'core://Page', id: 'meals',
    title: 'Meals', root: { type: 'core://Column', children: [
      { type: 'core://Box', height_dp: 64, padding_h_dp: 16, content: { type: 'core://Text', style: 'Headline', value_expr: 'Meals' } },
      { type: 'core://List', query_key: 'collection:meals', item_template: { type: 'core://Tappable',
        on_click: { intent: 'action.navigation.open', ref: recipe.id, params: { id: '$item.id' }, presentation: 'Push' },
        content: { type: 'core://Box', height_dp: 64, padding_h_dp: 16,
          content: { type: 'core://Text', style: 'Title', value_expr: '$item.name' } } } }] } } });
  await tool('set_handle', { handle: 'webcook' });
  const mealsReview = await tool('share_publicly', { layout: meals.id, slug: 'meals' });
  check(mealsReview.anyone_with_the_link_reads.layouts.length === 2, 'the review lists the list and the recipe it opens');
  const mealsLink = await tool('share_publicly', { layout: meals.id, slug: 'meals', confirm: mealsReview.review_id });
  check(mealsLink.short_url && mealsLink.short_url.endsWith('/@webcook/meals'), 'shared at ' + mealsLink.short_url);

  await page.goto(base + '/@webcook/meals');
  await page.waitForSelector('body.ready', { timeout: 60000 });
  await page.waitForTimeout(800);
  check(!green(await pixel(page, 195, 600)), 'the short path opens the list');
  await page.mouse.click(195, 96);
  await page.waitForTimeout(1200);
  await page.screenshot({ path: out + '/public-pushed.png' });
  check(green(await pixel(page, 195, 600)), 'tapping a meal pushes its recipe from the copy');
  await page.keyboard.press('Escape');
  await page.waitForTimeout(1200);
  check(!green(await pixel(page, 195, 600)), 'Back returns to the list');

  // A link opens in a new tab and the page keeps working. Desktop Chrome runs the page's frame (and a pointerleave)
  // inside window.open as the tab opens; the client used to be holding itself there, panicked, and froze.
  const links = await tool('create_layout', { title: 'Links', feeds, layout: { type: 'core://Page', id: 'links',
    title: 'Links', root: { type: 'core://Column', children: [
      { type: 'core://Tappable', on_click: { intent: 'action.link.open', url: 'https://shop.example/' },
        content: { type: 'core://Box', height_dp: 64, padding_h_dp: 16,
          content: { type: 'core://Text', style: 'Title', value_expr: 'View' } } },
      { type: 'core://List', query_key: 'collection:meals', item_template: { type: 'core://Tappable',
        on_click: { intent: 'action.navigation.open', ref: recipe.id, params: { id: '$item.id' }, presentation: 'Push' },
        content: { type: 'core://Box', height_dp: 64, padding_h_dp: 16,
          content: { type: 'core://Text', style: 'Title', value_expr: '$item.name' } } } }] } } });
  const linksReview = await tool('share_publicly', { layout: links.id, slug: 'links' });
  await tool('share_publicly', { layout: links.id, slug: 'links', confirm: linksReview.review_id });
  const shop = await browser.newPage({ viewport: { width: 390, height: 844 } });
  shop.on('pageerror', (e) => errors.push(e.message));
  await shop.context().route('https://shop.example/**', (r) => r.fulfill({ body: '<h1>Shop</h1>', contentType: 'text/html' }));
  await shop.addInitScript(() => {
    const pending = [];
    const raf = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = (cb) => {
      pending.push(cb);
      return raf((t) => {
        const i = pending.indexOf(cb);
        if (i >= 0) { pending.splice(i, 1); cb(t); }
      });
    };
    const open = window.open.bind(window);
    window.open = (...a) => {
      document.getElementById('construct').dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse', isPrimary: true }));
      for (const cb of pending.splice(0)) cb(performance.now());
      return open(...a);
    };
  });
  await shop.goto(base + '/@webcook/links');
  await shop.waitForSelector('body.ready', { timeout: 60000 });
  await shop.waitForTimeout(800);
  const popup = shop.waitForEvent('popup', { timeout: 5000 }).catch(() => null);
  await shop.mouse.click(40, 14); // on "View": a lone Tappable is as wide as what it holds
  const opened = await popup;
  check(opened && opened.url() === 'https://shop.example/', 'a link opens in a new tab');
  await shop.bringToFront();
  await shop.mouse.click(195, 96);
  await shop.waitForTimeout(1200);
  check(green(await pixel(shop, 195, 600)), 'the page still answers taps after the link opened');

  // A tap on a field on a phone keeps the hidden input focused, so the keyboard stays open: the tap's mousedown
  // used to blur it as soon as the tap focused it.
  const search = await tool('create_layout', { title: 'Search', feeds, layout: { type: 'core://Page', id: 'search',
    title: 'Search', root: { type: 'core://Column', padding_h_dp: 16, padding_v_dp: 16, children: [
      { type: 'core://TextField', field: 'q', placeholder: 'Search' }] } } });
  const searchReview = await tool('share_publicly', { layout: search.id, slug: 'search' });
  await tool('share_publicly', { layout: search.id, slug: 'search', confirm: searchReview.review_id });
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  const tapper = await phone.newPage();
  tapper.on('pageerror', (e) => errors.push(e.message));
  await tapper.goto(base + '/@webcook/search');
  await tapper.waitForSelector('body.ready', { timeout: 60000 });
  await tapper.waitForTimeout(800);
  await tapper.touchscreen.tap(195, 40);
  await tapper.waitForTimeout(300);
  check(await tapper.evaluate(() => document.activeElement && document.activeElement.id === 'construct-input'),
    'a tap on a field keeps the keyboard\'s input focused');
  await tapper.keyboard.type('soup');
  await tapper.waitForTimeout(300);
  await tapper.screenshot({ path: out + '/public-typed.png' });

  check(errors.length === 0, 'no page errors' + (errors.length ? ': ' + errors.join('; ') : ''));
  await browser.close();
})().catch((e) => {
  console.error('FAIL ' + e.message);
  process.exit(1);
});
