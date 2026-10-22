// Loader for the browser build: checks WebGPU, installs a data index into the
// game's virtual filesystem (skate-fs), prefetches the startup files, and
// starts the game on the player's click (audio needs a gesture). The data is
// the server's (skate-web-server --data) or, on a static host such as GitHub
// Pages, a folder the player picks on their own disk: those files stay local
// and are read through blob: URLs.
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

// Same policy as skate-web-server: files up to 4 MB, smallest first, 192 MB in all.
function prefetchList(files) {
  let spent = 0;
  return Object.entries(files)
    .filter(([, size]) => size <= 4 * 1048576)
    .sort(([a, x], [b, y]) => x - y || (a < b ? -1 : 1))
    .filter(([, size]) => (spent += size) <= 192 * 1048576)
    .map(([path]) => path);
}

// An index for a picked folder. Paths drop the folder's own name, so they are
// relative to it exactly as the server's are relative to --data.
function localIndex(fileList) {
  const files = {};
  const urls = {};
  for (const file of fileList) {
    const path = file.webkitRelativePath.split("/").slice(1).join("/");
    if (!path) continue;
    files[path] = file.size;
    urls[path] = URL.createObjectURL(file);
  }
  if (!("private/game.json" in files)) {
    throw new Error("That folder has no private/game.json. Choose the converted assets folder (data/installations/<id>/assets), not the game disc or the data folder above it.");
  }
  return { files, urls, prefetch: prefetchList(files), base_url: "" };
}

function pickFolder() {
  status("Choose your game data folder.");
  $("pick").hidden = false;
  return new Promise((resolve) => {
    const input = $("folder");
    input.addEventListener("change", function chosen() {
      try {
        const index = localIndex(input.files);
        input.removeEventListener("change", chosen);
        $("pick").hidden = true;
        resolve(index);
      } catch (error) {
        // Wrong folder: say why and let the player choose again.
        status(error.message);
        input.value = "";
      }
    });
  });
}

async function dataIndex() {
  const response = await fetch("data/index.json", { cache: "no-cache" }).catch(() => null);
  if (response?.ok) return response.json();
  // 404 on a static host, 503 from a server started without --data.
  if (!response || response.status === 404 || response.status === 503) return pickFolder();
  throw new Error(`data/index.json: HTTP ${response.status} — ${(await response.text()).trim()}`);
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
      const response = await fetch(index.urls?.[path] ?? `${index.base_url}${encode(path)}`);
      if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
      const bytes = new Uint8Array(await response.arrayBuffer());
      insert(path, bytes);
      done += bytes.length;
      progress(total ? done / total : 1);
      status(`Loading game data… ${(done / 1048576).toFixed(0)} / ${(total / 1048576).toFixed(0)} MB`);
    }
  }
  await Promise.all(Array.from({ length: 12 }, worker));
}

async function main() {
  try {
    const build = await fetchJson("build.json").catch(() => null);
    if (build?.revision) {
      const repository = build.repository || "https://github.com/SK8-ENGINE/skate-3-rust-engine";
      $("revision").textContent = ` (${build.revision.slice(0, 12)})`;
      $("source").href = `${repository}/tree/${build.revision}`;
    }
    await checkWebGpu();
    status("Loading the game…");
    const game = await import(`./pkg/skate3rust.js`);
    await game.default();
    status("Reading the game data index…");
    const index = await dataIndex();
    const count = game.skate_fs_install_index(JSON.stringify(index));
    status(`Loading game data… (${count} files)`);
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
