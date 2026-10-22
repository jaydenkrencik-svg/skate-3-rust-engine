//! Browser build glue: the page URL stands in for the command line, the
//! server's data directory for the asset root, and the console for stderr.
//! The loader page (web/static) installs the file index before `main` runs.
use std::{ffi::OsString, sync::Mutex};
use wasm_bindgen::prelude::*;

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

#[wasm_bindgen]
extern "C" {
    /// Defined by the loader; shows a startup failure on the page.
    #[wasm_bindgen(js_namespace = window, js_name = skateFatal)]
    fn skate_fatal(message: &str);
}

/// Query parameters as CLI arguments: `?test-world&difficulty=hardcore&map=maps/x.skate`
/// becomes `--test-world --difficulty hardcore --map /data/maps/x.skate`.
/// Map paths are relative to the data directory. Options the browser cannot
/// honour (`--assets`, `--verify`, `--net-*`, tool modes) are not accepted.
pub(crate) fn args() -> Vec<OsString> {
    const FLAGS: [&str; 3] = ["test-world", "start-paused", "mute"];
    const VALUES: [&str; 3] = ["map", "difficulty", "teleport"];
    let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) else {
        return Vec::new();
    };
    let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    for flag in FLAGS {
        if params.has(flag) {
            args.push(format!("--{flag}").into());
        }
    }
    for name in VALUES {
        let Some(value) = params.get(name).filter(|v| !v.is_empty()) else { continue };
        args.push(format!("--{name}").into());
        args.push(if name == "map" {
            format!("{}/{}", skate_fs::WEB_ROOT, value.trim_start_matches('/')).into()
        } else {
            value.into()
        });
    }
    args
}

/// Runs the game: no updater, setup tools, crash supervisor or trace capture.
/// Bevy's LogPlugin logs to the console. `app.run()` returns once the browser
/// owns the event loop; a startup error goes to the page instead.
#[wasm_bindgen]
pub fn skate_start() {
    if crate::run().is_error() {
        fatal(&last_error());
    }
}

/// Reports a startup error that ended `main` before the app could run.
pub(crate) fn fatal(message: &str) {
    web_sys::console::error_1(&message.into());
    skate_fatal(message);
}

/// `eprintln!` in the browser build (see main.rs). Machine-readable status
/// lines (`REPORT_META ...`, `MAP_LOAD_TIMING ...`) are logged; anything else
/// is an error and becomes the message a failed startup shows.
pub(crate) fn stderr(line: &str) {
    let marker = line.split(' ').next().is_some_and(|word| {
        word.len() > 1 && word.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
    });
    if marker {
        web_sys::console::log_1(&line.into());
    } else {
        web_sys::console::error_1(&line.into());
        *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = line.to_owned();
    }
}

/// The last line written through `eprintln!`; `main` prints its error last.
pub(crate) fn last_error() -> String {
    let line = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if line.is_empty() { "The game stopped during startup".into() } else { line }
}
