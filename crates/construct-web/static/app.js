// The web client's page (decision `web-client`): starts the WebAssembly client, and when there is no session
// asks for the pod code, which the server turns into a session cookie (the code is not kept here).
import init, { start, start_public } from "/web/construct_web.js";

const $ = (id) => document.getElementById(id);
const web = { "x-construct-client": "web" };

async function hasSession() {
  // A caller without a pod gets 401 here (the construct itself answers its "Open your pod" page).
  const r = await fetch("/v1/auth/sign-ins", { headers: web });
  return r.ok;
}

// A public link (/s/<link>, or a chosen /@<handle>/<slug>; decisions `public-web-links`,
// `public-links-share-the-closure`): its copy, read-only, for anyone; no pod, no sign-in.
const shared = location.pathname.match(/^(\/s\/pub-[a-z0-9]+|\/@[a-z0-9_-]+\/[a-z0-9-]+)\/?$/);

async function run() {
  if (shared) {
    await init();
    await start_public(shared[1]);
    $("loading").hidden = true;
    return;
  }
  if (!(await hasSession())) {
    $("loading").hidden = true;
    $("sign-in").hidden = false;
    $("code").focus();
    return;
  }
  await init();
  await start();
  $("loading").hidden = true;
}

$("code-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  $("code-error").textContent = "";
  const r = await fetch("/web/session", {
    method: "POST",
    headers: { ...web, "content-type": "application/json" },
    body: JSON.stringify({ code: $("code").value.trim() }),
  });
  if (r.ok) {
    location.reload();
  } else if (r.status === 429) {
    $("code-error").textContent = "Too many tries. Wait a minute and try again.";
  } else {
    $("code-error").textContent = "That code didn't open a pod.";
  }
});

run().catch((e) => {
  $("loading").textContent = "Couldn't open your pod: " + e;
});
