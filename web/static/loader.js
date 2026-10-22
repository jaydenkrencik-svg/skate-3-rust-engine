// Loader for the browser build: checks WebGPU, installs the server's data
// index into the game's virtual filesystem (skate-fs), prefetches the startup
// files, and starts the game on the player's click (audio needs a gesture).
const $ = (id) => document.getElementById(id);
const status = (text) => { $("status").textContent = text; };
const progress = (fraction) => { $("progress").style.width = `${Math.round(fraction * 100)}%`; };

let failed = false;
function fail(message) {
  failed = true;
  status("The game could not start.");
  $("start").hidden = true;
  $("overlay").hidden = false;
  $("error").classList.add("shown");
  const text = $("error-text");
  text.textContent = text.textContent ? `${text.textContent}\n\n${message}` : message;
}
// Called by the game (web.rs) for startup errors, and on Rust panics.
window.skateFatal = fail;
window.addEventListener("error", (event) => fail(event.message || String(event.error)));
window.addEventListener("unhandledrejection", (event) => fail(String(event.reason)));
const consoleError = console.error.bind(console);
console.error = (...args) => {
  consoleError(...args);
  const text = args.map(String).join(" ");
  if (text.includes("panicked at")) fail(text);
};
$("copy").addEventListener("click", () => navigator.clipboard?.writeText($("error-text").textContent));

async function checkWebGpu() {
  if (!navigator.gpu) {
    throw new Error("This browser has no WebGPU. Use a current Chrome or Edge (113+), Safari 26+, or Firefox 141+ on Windows, over https or localhost.");
  }
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new Error("WebGPU is present but no GPU adapter is available (check that hardware acceleration is enabled).");
}

async function fetchJson(url) {
  const response = await fetch(url, { cache: "no-cache" });
  if (!response.ok) {
    const detail = (await response.text()).trim();
    throw new Error(`${url}: HTTP ${response.status}${detail ? ` — ${detail}` : ""}`);
  }
  return response.json();
}

async function prefetch(index, insert) {
  const files = index.prefetch ?? [];
  const total = files.reduce((sum, path) => sum + (index.files[path] ?? 0), 0);
  let done = 0;
  let next = 0;
  const encode = (path) => path.split("/").map(encodeURIComponent).join("/");
  async function worker() {
    while (next < files.length && !failed) {
      const path = files[next++];
      const response = await fetch(`${index.base_url}${encode(path)}`);
      if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
      const bytes = new Uint8Array(await response.arrayBuffer());
      insert(path, bytes);
      done += bytes.length;
      progress(total ? done / total : 1);
      status(`Downloading game data… ${(done / 1048576).toFixed(0)} / ${(total / 1048576).toFixed(0)} MB`);
    }
  }
  await Promise.all(Array.from({ length: 12 }, worker));
}

async function main() {
  try {
    const build = await fetchJson("build.json").catch(() => null);
    if (build?.revision) {
      $("revision").textContent = ` (${build.revision.slice(0, 12)})`;
      $("source").href = `https://github.com/SK8-ENGINE/skate-3-rust-engine/tree/${build.revision}`;
    }
    await checkWebGpu();
    status("Loading the game…");
    const game = await import(`./pkg/skate3rust.js`);
    await game.default();
    status("Reading the game data index…");
    const index = await fetchJson("data/index.json");
    const count = game.skate_fs_install_index(JSON.stringify(index));
    status(`Downloading game data… (${count} files on the server)`);
    await prefetch(index, game.skate_fs_insert);
    if (failed) return;
    progress(1);
    status("Ready.");
    const start = $("start");
    start.hidden = false;
    start.focus();
    start.addEventListener("click", () => {
      $("overlay").hidden = true;
      $("skate-canvas").focus();
      game.skate_start();
    }, { once: true });
  } catch (error) {
    fail(error?.message ?? String(error));
  }
}

main();
