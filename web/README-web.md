# Browser build

The game compiles to WebAssembly (`wasm32-unknown-unknown`) and renders with
WebGPU. `skate-web-server` serves the page, the wasm package and **your own**
converted game data. Native Windows builds are unchanged: every browser change
is behind `cfg(target_arch = "wasm32")` or lives in a crate only the browser
build uses.

Status: milestone 1 done (the workspace builds for wasm32; the server, loader and
game startup run in a browser). Gameplay has not been verified; see [Status](#status).

## Requirements

- Rust (stable) with `rustup target add wasm32-unknown-unknown`
- `cargo install wasm-bindgen-cli --version 0.2.127 --locked` (must match the
  `wasm-bindgen` crate in Cargo.lock)
- Optional: `wasm-opt` from [binaryen](https://github.com/WebAssembly/binaryen)
  for a smaller download
- A browser with WebGPU: Chrome or Edge 113+, Safari 26+, Firefox 141+ (Windows).
  WebGPU, gamepads and `SharedArrayBuffer` need a secure context: `https://`
  or `http://localhost`.

## Game data

Skate 3 data belongs to Electronic Arts and is never part of this repository,
the wasm package or the Docker image. Run the normal Windows setup
(`skate3rust.exe`, which converts your own ISO or `default.xex`) and point the
server's `--data` at the converted asset directory, the one containing
`private/game.json`: `data/installations/<id>/assets` next to the executable,
or a development asset set such as `assets/`.

The server refuses to listen anywhere but loopback without a password, so the
data is never published by accident.

## Build and run

```powershell
web\build.ps1            # release: fat LTO, wasm-opt if available (slow)
web\build.ps1 -Dev       # dev profile, for iteration
cargo run --release -p skate-web-server -- --data C:\path\to\assets
```

Open <http://127.0.0.1:8080/>. Useful URL parameters replace the command line:

| URL | Native equivalent |
|---|---|
| `?test-world` | `--test-world` |
| `?map=maps/format-demo.skate` (relative to the data directory) | `--map …` |
| `?difficulty=hardcore` | `--difficulty hardcore` |
| `?teleport=ID`, `?start-paused`, `?mute` | same flags |

Server options (each also an environment variable):

| Flag | Env | Default |
|---|---|---|
| `--bind` | `SKATE_WEB_BIND` | `127.0.0.1:8080` |
| `--web` | `SKATE_WEB_DIR` | `web/dist` |
| `--data` | `SKATE_WEB_DATA` | none (page loads, game cannot start) |
| `--password` | `SKATE_WEB_PASSWORD` | none; required for non-loopback binds |

The password is HTTP Basic on every route; put the server behind a TLS reverse
proxy (Caddy, nginx) for anything but localhost.

### Keyboard

Without a gamepad in the first slot, the keyboard stands in for one (a pad
plugged in later takes over):

| Keys | Pad |
|---|---|
| W A S D | left stick |
| arrow keys | right stick (flick-it) |
| Space / Q / Shift / E | A / B / X / Y |
| J / K | left / right trigger |
| U / O | left / right bumper |
| C / V | left / right stick click |
| T F G H | d-pad up / left / down / right |
| Enter / Backspace | Start / Back |

Digital keys give full stick deflection, so analog-sensitive tricks are coarser
than on a pad.

### Docker

```sh
docker build -f web/Dockerfile -t skate3-web .
docker run -p 8080:8080 -v /path/to/assets:/data:ro -e SKATE_WEB_PASSWORD=change-me skate3-web
```

## How it works

| Piece | Native | Browser |
|---|---|---|
| Entry | `main` (updater, crash supervisor, trace capture) | `main` only installs the panic hook; the loader calls `skate_start()` after a click |
| Command line | `std::env::args` | URL query (`web.rs`) |
| Files | `std::fs` | `skate-fs`: server data index + prefetch + synchronous fetch fallback; writes go to an overlay persisted in `localStorage` (files ≤ 512 KB) |
| Bevy assets | `FileAssetReader` | default source fetches over HTTP; `mods`/`characters` sources read `skate-fs` |
| Renderer | Vulkan | WebGPU into `#skate-canvas` |
| Gamepads | SDL3 (XInput fallback) | Gamepad API, standard mapping → XInput-shaped state; keyboard drives slot 0 when no pad is there |
| Clock | `std::time` | `web-time` (identical types natively) |
| Map decode | zstd (C) | ruzstd (pure Rust) |
| Map switching | worker threads | inline on the page thread (the page pauses while a map loads) |
| Lua mods | mlua + vendored Lua 5.4 | not yet: packages load, every VM reports "not available in the browser build" |
| Multiplayer | UDP + Steam relay | not yet |
| Diagnostics | stderr | `eprintln!` → `console.error`; startup errors and panics appear on the page |

### Call sites that cannot work on `wasm32-unknown-unknown`

`std::fs`, `env::temp_dir`, `process::id`, `thread::sleep`, `Instant::now` and
`SystemTime::now` compile for wasm32 but fail or panic at run time, so the
compiler cannot find them; this is the audit.

| What | Where | Browser replacement |
|---|---|---|
| `std::fs::*` (~600 sites), `Path::{exists,is_file,is_dir,canonicalize}` (66) | skate-game, skate-data, skate-audio, skate-mods | `skate_fs::*` (re-exports `std::fs` natively) |
| `std::time::{Instant,SystemTime,UNIX_EPOCH}` (~40 files) | all crates | `web_time::*` |
| `std::env::temp_dir()` | `custom_models::library_path`, `multiplayer::appearance::cache_directory` | `skate_fs::temp_dir()` (`/tmp` in the overlay) |
| `std::process::id()` | `multiplayer::unique`, `map_transition`, `custom_models`, `skate_mods::archive` | `skate_fs::process_id()` |
| `std::thread::sleep` | `graphics_menu::pace` (FPS cap) | skipped; the browser paces frames |
| `std::thread::Builder::spawn` | `map_transition` (map loader + render loader) | runs inline |
| | `game_audio::native::prefetch` | already falls back to game-thread loads |
| | `frame_timing::log`, `game_audio::state_log` | opt-in diagnostics; spawn fails, logging is off |
| | `skate_data::texture_decode` | `available_parallelism` is 1, so no worker threads |
| `std::thread::spawn`, `Command`, `current_exe` | `crash_report`, `updater`, `setup` (ISO setup), `profiling` | not started in the browser |
| `Command` | `custom_models` (character importer), `modding::menu`, `multiplayer::transport` (Steam relay), `game_audio::native` (junction) | not reachable without their native tools; unchanged |
| `UdpSocket`, `socket2` | `skate-net`, `multiplayer::transport`, `appearance_transfer` | socket2 is native-only; UDP binds fail (multiplayer milestone) |
| SDL3, XInput | `input::platform`, `input::controller_kind` | Gamepad API (`platform::browser`, `controller_kind::from_browser`) |
| mlua / C Lua | `skate_mods::vm`, `archive` | stubbed VM (Lua milestone) |
| zstd (C) | `skate_data::skate_map` | ruzstd |
| `FileAssetReader` | `modding`, `custom_models` | `assets::directory_reader` |

## Status

Verified (Windows 11, Chromium-based browser pane, 2026-10-21):

- `cargo check --target wasm32-unknown-unknown` passes for every crate the
  game uses; the native build (bins and tests) still compiles.
- `webuild.ps1` produces the package; in the browser the loader passes the
  WebGPU check, instantiates the wasm, installs the data index, prefetches, and
  `skate_start()` runs the game's startup code: URL arguments, `/data` through
  `skate-fs`, then the first game-data read. With no converted data that read
  fails and the error appears on the page, as intended.
- Native tests: skate-game 486 passed (1 failure, also failing upstream:
  `setup::tests::pipelines_accept_valid_group_outputs_when_fingerprint_changes`),
  skate-audio, skate-data, skate-net, skate-dynamics, skate-mods pass except
  tests that need assets absent from the checkout (`skyline.glb`) and one
  upstream skate-core failure (`collision_feedback_tests::a_moving_group_8...`).
- `skate-web-server` unit tests: serving, index, isolation headers, password gate.
- Release package: 59 MB wasm, 16 MB gzipped on the wire (without wasm-opt).

Not yet verified or not done (later milestones from the port plan):

- Gameplay in a browser: needs converted Skate 3 data, which this repository
  cannot ship and this machine does not have. Rendering, custom WGSL under
  WebGPU validation (storage buffers in the retail materials, Bevy's bindless
  and occlusion paths), audio output and gamepad play are untested until then.
- A "Web" graphics preset, AudioWorklet output (Bevy's WebAudio output is used
  as is), progress display for maps loaded after startup.
- Lua mods, WebSocket multiplayer, ISO upload/conversion on the server.
- Caching asset groups by pipeline hash (the browser's HTTP cache revalidates today).
- Map textures are decoded to RGBA by skate-data, so BC texture support is
  not required for maps; other compressed textures have not been audited.
