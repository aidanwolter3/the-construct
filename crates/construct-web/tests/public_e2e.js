// A public link in Chromium (decision `public-web-links`) against a running server with CONSTRUCT_PUBLIC_WEB=1
// (scripts/web_e2e.sh starts one): an agent makes a made-up recipes collection and layout over MCP, reviews and
// shares it; a visitor with no pod opens the page; a new row reaches it; a field the page doesn't show never does;
// unsharing stops it. Screenshots go to the directory given.
//   node crates/construct-web/tests/public_e2e.js <base url> <mcp key> <out dir>
const { chromium } = require('playwright');
const [base, key, out] = process.argv.slice(2);

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
  check(errors.length === 0, 'no page errors' + (errors.length ? ': ' + errors.join('; ') : ''));
  await browser.close();
})().catch((e) => {
  console.error('FAIL ' + e.message);
  process.exit(1);
});
