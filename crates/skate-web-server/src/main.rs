//! Web server for the browser build.
//!
//! Serves three things: the loader page and wasm package (`--web`), the
//! operator's converted game data (`--data`, the `data` directory that the
//! normal setup writes from the operator's own Skate 3 disc), and an index of
//! that data which the game's virtual filesystem (skate-fs) needs before it
//! starts. Skate 3 data is Electronic Arts' content: this server never ships
//! any, and it refuses to listen beyond this machine without a password.
//!
//! Every response carries the cross-origin isolation headers WebGPU pages need
//! for `SharedArrayBuffer` (wasm threads, a shared audio ring buffer).
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::Engine;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};

/// Files at most this large are prefetched by the loader before the game
/// starts, smallest first, until the budget is spent. Anything else is fetched
/// when the game first reads it.
const PREFETCH_FILE_LIMIT: u64 = 4 * 1024 * 1024;
const PREFETCH_BUDGET: u64 = 192 * 1024 * 1024;

#[derive(Debug, PartialEq)]
struct Options {
    bind: SocketAddr,
    web: PathBuf,
    data: Option<PathBuf>,
    password: Option<String>,
}

const USAGE: &str = "Usage: skate-web-server [--bind ADDRESS:PORT] [--web DIRECTORY] [--data DIRECTORY] [--password PASSWORD]
  --bind      listen address (default 127.0.0.1:8080; SKATE_WEB_BIND)
  --web       built loader + wasm package (default web/dist; SKATE_WEB_DIR)
  --data      converted game data, the setup's `data` directory (SKATE_WEB_DATA)
  --password  HTTP Basic password for every route (SKATE_WEB_PASSWORD);
              required unless --bind is a loopback address";

fn parse(args: impl IntoIterator<Item = String>, env: impl Fn(&str) -> Option<String>) -> Result<Options, String> {
    let mut options = Options {
        bind: env("SKATE_WEB_BIND").as_deref().unwrap_or("127.0.0.1:8080").parse().map_err(|_| "Invalid SKATE_WEB_BIND")?,
        web: env("SKATE_WEB_DIR").map_or_else(|| PathBuf::from("web/dist"), PathBuf::from),
        data: env("SKATE_WEB_DATA").map(PathBuf::from),
        password: env("SKATE_WEB_PASSWORD").filter(|p| !p.is_empty()),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} requires a value\n{USAGE}"));
        match arg.as_str() {
            "--bind" => options.bind = value()?.parse().map_err(|_| "Invalid --bind address")?,
            "--web" => options.web = value()?.into(),
            "--data" => options.data = Some(value()?.into()),
            "--password" => options.password = Some(value()?).filter(|p| !p.is_empty()),
            "--help" | "-h" => return Err(USAGE.into()),
            _ => return Err(format!("Unknown argument {arg:?}\n{USAGE}")),
        }
    }
    if options.password.is_none() && !options.bind.ip().is_loopback() {
        return Err(format!(
            "Refusing to serve game data on {} without a password: set --password or SKATE_WEB_PASSWORD, or bind to 127.0.0.1",
            options.bind
        ));
    }
    Ok(options)
}

/// The data directory as the browser's virtual filesystem sees it.
#[derive(serde::Serialize)]
struct DataIndex {
    /// `/`-separated paths relative to the data directory, with sizes.
    files: BTreeMap<String, u64>,
    /// Files the loader fetches before starting the game.
    prefetch: Vec<String>,
    /// Where the files are served from, relative to the page.
    base_url: &'static str,
}

fn scan(root: &Path) -> std::io::Result<DataIndex> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, u64>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                walk(root, &path, files)?;
            } else if kind.is_file() {
                let Ok(relative) = path.strip_prefix(root) else { continue };
                let Some(relative) = relative.to_str() else { continue };
                files.insert(relative.replace('\\', "/"), entry.metadata()?.len());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files)?;
    let mut small: Vec<_> = files.iter().filter(|(_, size)| **size <= PREFETCH_FILE_LIMIT).collect();
    small.sort_by_key(|(path, size)| (**size, (*path).clone()));
    let mut spent = 0;
    let prefetch = small
        .into_iter()
        .take_while(|(_, size)| {
            spent += **size;
            spent <= PREFETCH_BUDGET
        })
        .map(|(path, _)| path.clone())
        .collect();
    Ok(DataIndex { files, prefetch, base_url: "data/" })
}

struct AppState {
    data: Option<PathBuf>,
    /// The serialized index, built on first request.
    index: RwLock<Option<Arc<[u8]>>>,
    password: Option<String>,
}

async fn data_index(State(state): State<Arc<AppState>>) -> Response {
    let Some(root) = state.data.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "No game data is configured on this server (start it with --data).").into_response();
    };
    let json = |bytes: Arc<[u8]>| ([(header::CONTENT_TYPE, "application/json")], Body::from(bytes.to_vec())).into_response();
    if let Some(index) = state.index.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return json(index);
    }
    match tokio::task::spawn_blocking(move || scan(&root)).await {
        Ok(Ok(index)) => {
            let bytes: Arc<[u8]> = serde_json::to_vec(&index).expect("index serializes").into();
            *state.index.write().unwrap_or_else(|e| e.into_inner()) = Some(bytes.clone());
            json(bytes)
        }
        Ok(Err(error)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not read the game data directory: {error}")).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn authorized(headers: &HeaderMap, password: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return false };
    let Some(encoded) = value.strip_prefix("Basic ") else { return false };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else { return false };
    // Any user name; only the password counts. Compare without early exit.
    let supplied = decoded.splitn(2, |&b| b == b':').nth(1).unwrap_or_default();
    let expected = password.as_bytes();
    supplied.len() == expected.len() && supplied.iter().zip(expected).fold(0, |acc, (a, b)| acc | (a ^ b)) == 0
}

async fn require_password(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    match &state.password {
        Some(password) if !authorized(request.headers(), password) => Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, "Basic realm=\"Skate 3 Rust Engine\", charset=\"UTF-8\"")
            .body(Body::from("Password required"))
            .expect("static response"),
        _ => next.run(request).await,
    }
}

fn router(options: &Options) -> Router {
    let state = Arc::new(AppState {
        data: options.data.clone(),
        index: RwLock::new(None),
        password: options.password.clone(),
    });
    let header = |name: &'static str, value: &'static str| {
        SetResponseHeaderLayer::overriding(HeaderName::from_static(name), HeaderValue::from_static(value))
    };
    // Revalidate everything: the package is not content-hashed, and data
    // changes when the operator reruns setup.
    let web = ServeDir::new(&options.web)
        .precompressed_br()
        .precompressed_gzip()
        .not_found_service(ServeFile::new(options.web.join("index.html")));
    let mut app = Router::new()
        .route("/data/index.json", get(data_index))
        .route("/healthz", get(|| async { "ok" }));
    if let Some(data) = &options.data {
        app = app.nest_service("/data", ServeDir::new(data));
    }
    app.fallback_service(web)
        .layer(middleware::from_fn_with_state(state.clone(), require_password))
        .layer(CompressionLayer::new().br(true).gzip(true))
        .layer(header("cross-origin-opener-policy", "same-origin"))
        .layer(header("cross-origin-embedder-policy", "require-corp"))
        .layer(header("cross-origin-resource-policy", "same-origin"))
        .layer(header("cache-control", "no-cache"))
        .layer(header("x-content-type-options", "nosniff"))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    let options = match parse(std::env::args().skip(1), |key| std::env::var(key).ok()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if !options.web.join("index.html").is_file() {
        eprintln!("{}: no index.html; build the browser package first (web/build.ps1)", options.web.display());
        std::process::exit(2);
    }
    match &options.data {
        Some(data) if !data.is_dir() => {
            eprintln!("{}: game data directory not found", data.display());
            std::process::exit(2);
        }
        Some(data) => eprintln!("Serving game data from {}", data.display()),
        None => eprintln!("No --data directory: the page will load, but the game cannot start"),
    }
    let listener = match tokio::net::TcpListener::bind(options.bind).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Cannot listen on {}: {error}", options.bind);
            std::process::exit(1);
        }
    };
    eprintln!("SKATE_WEB_READY http://{}/{}", options.bind, if options.password.is_some() { " (password protected)" } else { "" });
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(error) = axum::serve(listener, router(&options)).with_graceful_shutdown(shutdown).await {
        eprintln!("Server error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skate-web-server-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(name: &str, password: Option<&str>) -> (Options, PathBuf) {
        let root = temp_dir(name);
        let web = root.join("web");
        let data = root.join("data");
        std::fs::create_dir_all(web.join("pkg")).unwrap();
        std::fs::create_dir_all(data.join("anim")).unwrap();
        std::fs::write(web.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(web.join("pkg/game_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
        std::fs::write(data.join("anim/a.bin"), [1u8; 10]).unwrap();
        std::fs::write(data.join("big.skate"), vec![0u8; (PREFETCH_FILE_LIMIT + 1) as usize]).unwrap();
        let options = Options { bind: "127.0.0.1:0".parse().unwrap(), web, data: Some(data), password: password.map(str::to_owned) };
        (options, root)
    }

    async fn get(app: &Router, uri: &str, auth: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = Request::get(uri);
        if let Some(auth) = auth {
            let encoded = base64::engine::general_purpose::STANDARD.encode(auth);
            request = request.header(header::AUTHORIZATION, format!("Basic {encoded}"));
        }
        let response = app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
        let (parts, body) = response.into_parts();
        (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
    }

    #[test]
    fn public_binding_requires_a_password() {
        let none = |_: &str| None;
        assert!(parse(["--bind".into(), "0.0.0.0:80".into()], none).unwrap_err().contains("without a password"));
        let options = parse(["--bind".into(), "0.0.0.0:80".into(), "--password".into(), "pw".into()], none).unwrap();
        assert_eq!(options.password.as_deref(), Some("pw"));
        assert!(parse(Vec::<String>::new(), none).unwrap().bind.ip().is_loopback());
        assert!(parse(["--nope".into()], none).is_err());
    }

    #[tokio::test]
    async fn serves_index_package_and_data_with_isolation_headers() {
        let (options, root) = fixture("serve", None);
        let app = router(&options);
        let (status, headers, body) = get(&app, "/", None).await;
        assert_eq!((status, body.as_slice()), (StatusCode::OK, b"<!doctype html>".as_slice()));
        assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
        assert_eq!(headers["cross-origin-embedder-policy"], "require-corp");
        let (status, headers, _) = get(&app, "/pkg/game_bg.wasm", None).await;
        assert_eq!((status, &headers[header::CONTENT_TYPE]), (StatusCode::OK, &HeaderValue::from_static("application/wasm")));
        let (status, _, body) = get(&app, "/data/anim/a.bin", None).await;
        assert_eq!((status, body), (StatusCode::OK, vec![1u8; 10]));
        let (status, _, body) = get(&app, "/data/index.json", None).await;
        assert_eq!(status, StatusCode::OK);
        let index: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(index["files"]["anim/a.bin"], 10);
        assert_eq!(index["prefetch"], serde_json::json!(["anim/a.bin"]));
        let (status, _, _) = get(&app, "/data/../web/index.html", None).await;
        assert_ne!(status, StatusCode::OK);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn password_protects_every_route() {
        let (options, root) = fixture("auth", Some("secret"));
        let app = router(&options);
        for uri in ["/", "/data/index.json", "/data/anim/a.bin", "/pkg/game_bg.wasm"] {
            let (status, headers, _) = get(&app, uri, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
            assert!(headers.contains_key(header::WWW_AUTHENTICATE));
            assert_eq!(get(&app, uri, Some("skater:wrong")).await.0, StatusCode::UNAUTHORIZED, "{uri}");
            assert_eq!(get(&app, uri, Some("skater:secret")).await.0, StatusCode::OK, "{uri}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn missing_data_is_reported_not_fatal() {
        let (mut options, root) = fixture("nodata", None);
        options.data = None;
        let app = router(&options);
        let (status, _, body) = get(&app, "/data/index.json", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(String::from_utf8(body).unwrap().contains("--data"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
