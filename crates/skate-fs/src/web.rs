//! Browser virtual filesystem (see the crate docs).
//!
//! Layers, first match wins: the write overlay (including deletions), the
//! read cache (prefetched or previously fetched files), then the server, for
//! paths the installed index lists under [`WEB_ROOT`]. Directory structure
//! comes from the index plus overlay writes; the server is never asked for a
//! listing.
use crate::WEB_ROOT;
use base64::Engine;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::Duration,
};
use wasm_bindgen::prelude::*;
use web_time::{SystemTime, UNIX_EPOCH};

/// Files larger than this stay in memory only; `localStorage` is ~5 MB per origin.
const PERSIST_LIMIT: usize = 512 * 1024;
const STORAGE_FILE: &str = "skate-fs:file:";
const STORAGE_DELETED: &str = "skate-fs:deleted:";

#[derive(Default)]
struct State {
    /// Server files under `WEB_ROOT`, by normalized absolute path, with sizes.
    index: BTreeMap<String, u64>,
    /// Every ancestor directory of an indexed or written file.
    dirs: BTreeSet<String>,
    /// Bytes already read from the server.
    cache: HashMap<String, Arc<[u8]>>,
    /// Local writes; `None` deletes a server file.
    overlay: BTreeMap<String, Option<Arc<[u8]>>>,
    /// Base URL the server's data directory is served from (ends with '/').
    base_url: String,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            let mut state = State { base_url: "data/".into(), ..State::default() };
            state.dirs.insert("/".into());
            state.dirs.insert(WEB_ROOT.into());
            Mutex::new(state)
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Absolute, `/`-separated, without `.`/`..`; relative paths resolve against `/`.
fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().replace('\\', "/")),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let joined = parts.join("/");
    let mut out = String::with_capacity(joined.len() + 1);
    out.push('/');
    out.push_str(joined.trim_matches('/'));
    out
}

fn parent(path: &str) -> Option<&str> {
    let at = path.rfind('/')?;
    Some(if at == 0 { "/" } else { &path[..at] })
}

fn add_ancestors(dirs: &mut BTreeSet<String>, path: &str) {
    let mut current = parent(path);
    while let Some(dir) = current {
        if !dirs.insert(dir.to_string()) {
            break;
        }
        current = parent(dir);
    }
}

fn not_found(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path}: not found"))
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn persist(path: &str, bytes: Option<&[u8]>) {
    let Some(storage) = storage() else { return };
    match bytes {
        Some(bytes) if bytes.len() <= PERSIST_LIMIT => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let _ = storage.set_item(&format!("{STORAGE_FILE}{path}"), &encoded);
            let _ = storage.remove_item(&format!("{STORAGE_DELETED}{path}"));
        }
        Some(_) => {
            let _ = storage.remove_item(&format!("{STORAGE_FILE}{path}"));
        }
        None => {
            let _ = storage.remove_item(&format!("{STORAGE_FILE}{path}"));
            let _ = storage.set_item(&format!("{STORAGE_DELETED}{path}"), "1");
        }
    }
}

fn restore(state: &mut State) {
    let Some(storage) = storage() else { return };
    let count = storage.length().unwrap_or(0);
    for i in 0..count {
        let Ok(Some(key)) = storage.key(i) else { continue };
        if let Some(path) = key.strip_prefix(STORAGE_FILE) {
            let Ok(Some(value)) = storage.get_item(&key) else { continue };
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(value) {
                add_ancestors(&mut state.dirs, path);
                state.overlay.insert(path.to_string(), Some(bytes.into()));
            }
        } else if let Some(path) = key.strip_prefix(STORAGE_DELETED) {
            state.overlay.entry(path.to_string()).or_insert(None);
        }
    }
}

#[derive(serde::Deserialize)]
struct IndexFile {
    /// Paths relative to the data directory, `/`-separated, with sizes.
    files: BTreeMap<String, u64>,
    #[serde(default)]
    base_url: Option<String>,
}

/// Called by the loader before the game starts, with the server's
/// `index.json`. Also restores persisted writes.
#[wasm_bindgen]
pub fn skate_fs_install_index(json: &str) -> Result<usize, JsValue> {
    let index: IndexFile = serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let mut state = state();
    if let Some(base) = index.base_url {
        state.base_url = if base.ends_with('/') { base } else { format!("{base}/") };
    }
    for (relative, size) in index.files {
        let path = normalize(&Path::new(WEB_ROOT).join(relative));
        add_ancestors(&mut state.dirs, &path);
        state.index.insert(path, size);
    }
    restore(&mut state);
    Ok(state.index.len())
}

/// Hands the VFS a file the loader fetched asynchronously, so the first read
/// does not block on a synchronous request.
#[wasm_bindgen]
pub fn skate_fs_insert(relative: &str, bytes: Vec<u8>) {
    let path = normalize(&Path::new(WEB_ROOT).join(relative));
    state().cache.insert(path, bytes.into());
}

/// Bytes the VFS currently holds in its read cache (diagnostics).
#[wasm_bindgen]
pub fn skate_fs_cached_bytes() -> f64 {
    state().cache.values().map(|b| b.len() as f64).sum()
}

fn url_for(base: &str, path: &str) -> Option<String> {
    let relative = path.strip_prefix(WEB_ROOT)?.trim_start_matches('/');
    let encoded: Vec<String> = relative.split('/').map(|s| String::from(js_sys::encode_uri_component(s))).collect();
    Some(format!("{base}{}", encoded.join("/")))
}

/// Synchronous binary GET. Browsers only allow text responses on synchronous
/// requests from a window, so the bytes travel as x-user-defined characters.
fn fetch_sync(url: &str) -> io::Result<Vec<u8>> {
    let error = |what: &str| io::Error::other(format!("{url}: {what}"));
    let request = web_sys::XmlHttpRequest::new().map_err(|_| error("cannot create request"))?;
    request.open_with_async("GET", url, false).map_err(|_| error("cannot open request"))?;
    request.override_mime_type("text/plain; charset=x-user-defined").map_err(|_| error("cannot set MIME type"))?;
    request.send().map_err(|_| error("request failed"))?;
    match request.status().unwrap_or(0) {
        200 => {}
        404 => return Err(io::Error::new(io::ErrorKind::NotFound, format!("{url}: not found"))),
        401 | 403 => return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{url}: not authorized"))),
        status => return Err(error(&format!("HTTP {status}"))),
    }
    let text = request.response_text().ok().flatten().unwrap_or_default();
    Ok(text.encode_utf16().map(|unit| unit as u8).collect())
}

fn load(path: &str) -> io::Result<Arc<[u8]>> {
    let url = {
        let state = state();
        if let Some(entry) = state.overlay.get(path) {
            return entry.clone().ok_or_else(|| not_found(path));
        }
        if let Some(bytes) = state.cache.get(path) {
            return Ok(bytes.clone());
        }
        if !state.index.contains_key(path) {
            return Err(not_found(path));
        }
        url_for(&state.base_url, path).ok_or_else(|| not_found(path))?
    };
    let bytes: Arc<[u8]> = fetch_sync(&url)?.into();
    state().cache.insert(path.to_string(), bytes.clone());
    Ok(bytes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File(u64),
    Dir,
}

fn kind(path: &str) -> Option<Kind> {
    let state = state();
    match state.overlay.get(path) {
        Some(Some(bytes)) => return Some(Kind::File(bytes.len() as u64)),
        Some(None) => return None,
        None => {}
    }
    if let Some(&size) = state.index.get(path) {
        return Some(Kind::File(size));
    }
    state.dirs.contains(path).then_some(Kind::Dir)
}

fn store(path: &str, bytes: Vec<u8>) -> io::Result<()> {
    if matches!(kind(path), Some(Kind::Dir)) {
        return Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{path}: is a directory")));
    }
    persist(path, Some(&bytes));
    let mut state = state();
    add_ancestors(&mut state.dirs, path);
    state.cache.remove(path);
    state.overlay.insert(path.to_string(), Some(bytes.into()));
    Ok(())
}

fn delete(path: &str) {
    persist(path, None);
    let mut state = state();
    state.cache.remove(path);
    state.overlay.insert(path.to_string(), None);
}

fn children(dir: &str) -> Vec<(String, Kind)> {
    let prefix = if dir == "/" { "/".to_string() } else { format!("{dir}/") };
    let direct = |path: &str| path.strip_prefix(&prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
    let state = state();
    let mut out: BTreeMap<String, Kind> = BTreeMap::new();
    for (path, size) in state.index.range(prefix.clone()..) {
        if !path.starts_with(&prefix) {
            break;
        }
        if direct(path) {
            out.insert(path.clone(), Kind::File(*size));
        }
    }
    for path in state.dirs.range(prefix.clone()..) {
        if !path.starts_with(&prefix) {
            break;
        }
        if direct(path) {
            out.insert(path.clone(), Kind::Dir);
        }
    }
    for (path, entry) in state.overlay.range(prefix.clone()..) {
        if !path.starts_with(&prefix) {
            break;
        }
        if direct(path) {
            match entry {
                Some(bytes) => {
                    out.insert(path.clone(), Kind::File(bytes.len() as u64));
                }
                None => {
                    out.remove(path);
                }
            }
        }
    }
    out.into_iter().collect()
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    load(&normalize(path.as_ref())).map(|bytes| bytes.to_vec())
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    store(&normalize(path.as_ref()), contents.as_ref().to_vec())
}

pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    if matches!(kind(&path), Some(Kind::File(_))) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{path}: is a file")));
    }
    let mut state = state();
    add_ancestors(&mut state.dirs, &path);
    state.dirs.insert(path);
    Ok(())
}

pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let normalized = normalize(path.as_ref());
    if kind(&normalized).is_some() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{normalized}: exists")));
    }
    create_dir_all(path)
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    match kind(&path) {
        Some(Kind::File(_)) => {
            delete(&path);
            Ok(())
        }
        Some(Kind::Dir) => Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{path}: is a directory"))),
        None => Err(not_found(&path)),
    }
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    if kind(&path) != Some(Kind::Dir) {
        return Err(not_found(&path));
    }
    for (child, kind) in children(&path) {
        match kind {
            Kind::File(_) => delete(&child),
            Kind::Dir => remove_dir_all(&child)?,
        }
    }
    state().dirs.remove(&path);
    Ok(())
}

pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    if !children(&path).is_empty() {
        return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, format!("{path}: not empty")));
    }
    remove_dir_all(&path)
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let from = normalize(from.as_ref());
    let to = normalize(to.as_ref());
    match kind(&from) {
        Some(Kind::File(_)) => {
            let bytes = load(&from)?.to_vec();
            store(&to, bytes)?;
            delete(&from);
            Ok(())
        }
        Some(Kind::Dir) => {
            create_dir_all(&to)?;
            for (child, _) in children(&from) {
                let name = child.rsplit('/').next().unwrap_or_default();
                rename(&child, format!("{to}/{name}"))?;
            }
            state().dirs.remove(&from);
            Ok(())
        }
        None => Err(not_found(&from)),
    }
}

pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let bytes = read(from)?;
    let len = bytes.len() as u64;
    write(to, bytes)?;
    Ok(len)
}

pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = normalize(path.as_ref());
    kind(&path).map(|_| PathBuf::from(&path)).ok_or_else(|| not_found(&path))
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    kind(&normalize(path.as_ref())).is_some()
}

pub fn is_file(path: impl AsRef<Path>) -> bool {
    matches!(kind(&normalize(path.as_ref())), Some(Kind::File(_)))
}

pub fn is_dir(path: impl AsRef<Path>) -> bool {
    matches!(kind(&normalize(path.as_ref())), Some(Kind::Dir))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType(bool);

impl FileType {
    pub fn is_file(&self) -> bool {
        !self.0
    }
    pub fn is_dir(&self) -> bool {
        self.0
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug)]
pub struct Metadata {
    len: u64,
    dir: bool,
}

impl Metadata {
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_file(&self) -> bool {
        !self.dir
    }
    pub fn is_dir(&self) -> bool {
        self.dir
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
    pub fn file_type(&self) -> FileType {
        FileType(self.dir)
    }
    /// Server files carry no timestamps; report a fixed time so freshness
    /// checks compare equal instead of failing.
    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(UNIX_EPOCH + Duration::from_secs(1))
    }
}

pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let path = normalize(path.as_ref());
    match kind(&path) {
        Some(Kind::File(len)) => Ok(Metadata { len, dir: false }),
        Some(Kind::Dir) => Ok(Metadata { len: 0, dir: true }),
        None => Err(not_found(&path)),
    }
}

pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    metadata(path)
}

pub struct DirEntry {
    path: PathBuf,
    kind: Kind,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> OsString {
        self.path.file_name().map(OsString::from).unwrap_or_default()
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(match self.kind {
            Kind::File(len) => Metadata { len, dir: false },
            Kind::Dir => Metadata { len: 0, dir: true },
        })
    }
    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(FileType(self.kind == Kind::Dir))
    }
}

pub struct ReadDir(std::vec::IntoIter<(String, Kind)>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(path, kind)| Ok(DirEntry { path: PathBuf::from(path), kind }))
    }
}

pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let path = normalize(path.as_ref());
    if kind(&path) != Some(Kind::Dir) {
        return Err(not_found(&path));
    }
    Ok(ReadDir(children(&path).into_iter()))
}

/// An open file: the whole content in memory, written back on flush/drop.
pub struct File {
    path: String,
    data: io::Cursor<Vec<u8>>,
    writable: bool,
    dirty: bool,
}

impl File {
    pub fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path)
    }
    pub fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().write(true).create(true).truncate(true).open(path)
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(Metadata { len: self.data.get_ref().len() as u64, dir: false })
    }
    pub fn set_len(&mut self, size: u64) -> io::Result<()> {
        self.data.get_mut().resize(size as usize, 0);
        self.dirty = true;
        Ok(())
    }
    pub fn sync_all(&self) -> io::Result<()> {
        Ok(())
    }
    pub fn sync_data(&self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.data.read(buf)
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.data.seek(pos)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.writable {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "file opened read-only"));
        }
        self.dirty = true;
        self.data.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.dirty {
            store(&self.path, self.data.get_ref().clone())?;
            self.dirty = false;
        }
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn read(&mut self, value: bool) -> &mut Self {
        self.read = value;
        self
    }
    pub fn write(&mut self, value: bool) -> &mut Self {
        self.write = value;
        self
    }
    pub fn append(&mut self, value: bool) -> &mut Self {
        self.append = value;
        self
    }
    pub fn truncate(&mut self, value: bool) -> &mut Self {
        self.truncate = value;
        self
    }
    pub fn create(&mut self, value: bool) -> &mut Self {
        self.create = value;
        self
    }
    pub fn create_new(&mut self, value: bool) -> &mut Self {
        self.create_new = value;
        self
    }
    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let path = normalize(path.as_ref());
        let existing = kind(&path);
        if existing == Some(Kind::Dir) {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{path}: is a directory")));
        }
        if self.create_new && existing.is_some() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{path}: exists")));
        }
        let writable = self.write || self.append;
        let data = match existing {
            Some(_) if self.truncate && writable => Vec::new(),
            Some(_) => load(&path)?.to_vec(),
            None if writable && (self.create || self.create_new) => Vec::new(),
            None => return Err(not_found(&path)),
        };
        let mut cursor = io::Cursor::new(data);
        if self.append {
            cursor.seek(SeekFrom::End(0))?;
        }
        Ok(File { path, data: cursor, writable, dirty: writable && existing.is_none() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_absolute_and_resolves_dots() {
        assert_eq!(normalize(Path::new("/data/./a/../b.txt")), "/data/b.txt");
        assert_eq!(normalize(Path::new("data\\x")), "/data/x");
        assert_eq!(parent("/data/b.txt"), Some("/data"));
        assert_eq!(parent("/data"), Some("/"));
    }
}
