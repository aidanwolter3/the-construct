// The web client's page (decision `web-client`): starts the WebAssembly client, and when there is no session
// asks for the pod code, which the server turns into a session cookie (the code is not kept here).
import init, { start } from "/web/construct_web.js";

const $ = (id) => document.getElementById(id);
const web = { "x-construct-client": "web" };

async function hasSession() {
  // A caller without a pod gets 401 here (the construct itself answers its "Open your pod" page).
  const r = await fetch("/v1/auth/sign-ins", { headers: web });
  return r.ok;
}

async function run() {
  if (!(await hasSession())) {
    $("loading").hidden = true;
    $("sign-in").hidden = false;
    $("code").focus();
    return;
  }
  await init();
  await start();
  $("loading").hidden = true;
  $("log-out").hidden = false;
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

$("log-out").addEventListener("click", async () => {
  await fetch("/web/logout", { method: "POST", headers: web });
  location.reload();
});

run().catch((e) => {
  $("loading").textContent = "Couldn't open your pod: " + e;
});
