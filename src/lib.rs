//! POM plugin that brings Ghostly into the POM: its own web app as the screen,
//! with the profile and wallets in the browser, and a member of the network's
//! Ghostly group on every node.
//!
//! Both are built from Ghostly's latest release, unmodified (`runtime`). The
//! node's member is driven by `chat`; `upstream` serves the web app and the
//! POM bar's routes. The node's identity and the group's link travel through
//! the node's loopback API for plugins (`node_api`).

pub mod chat;
pub mod node_api;
pub mod runtime;
pub mod upstream;

use serde_json::{json, Value};
use std::ffi::{c_char, c_void};
use std::path::PathBuf;
use std::sync::Arc;

const ABI_VERSION: u32 = 1;
static VERSION: &[u8] = b"0.1.0\0";
const UI_MANIFEST: &str = include_str!("../ui/manifest.json");
/// Protocol of the events the POM delivers through the `host.event` operation.
const EVENTS_PROTOCOL: &str = "pom-plugin-events/v1";
/// Directory of this plugin's data, beside the plugin database the POM gives it.
const PLUGIN_DIR: &str = "ghostly";

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ByteSlice {
    pub ptr: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ByteBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HostCallbacks {
    pub log: Option<unsafe extern "C" fn(message: ByteSlice)>,
}

pub type PluginHandle = *mut c_void;
pub type CreateFn = unsafe extern "C" fn(HostCallbacks, ByteSlice) -> PluginHandle;
pub type IngestFn = unsafe extern "C" fn(PluginHandle, ByteSlice) -> i32;
pub type QueryFn = unsafe extern "C" fn(PluginHandle, ByteSlice) -> ByteBuffer;
pub type FreeBufferFn = unsafe extern "C" fn(ByteBuffer);
pub type ShutdownFn = unsafe extern "C" fn(PluginHandle);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PluginApiV1 {
    pub abi_version: u32,
    pub plugin_version: *const c_char,
    pub capabilities: u64,
    pub create: Option<CreateFn>,
    pub ingest: Option<IngestFn>,
    pub query: Option<QueryFn>,
    pub free_buffer: Option<FreeBufferFn>,
    pub shutdown: Option<ShutdownFn>,
}

unsafe impl Sync for PluginApiV1 {}

struct PluginState {
    chat: Arc<chat::Chat>,
    upstream: Option<upstream::Upstream>,
}

/// `<directory of POM_PLUGIN_DB>/ghostly`, or the working directory's.
fn plugin_directory() -> PathBuf {
    std::env::var_os("POM_PLUGIN_DB")
        .map(PathBuf::from)
        .and_then(|database| database.parent().map(PathBuf::from))
        .filter(|parent| !parent.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(PLUGIN_DIR)
}

impl PluginState {
    fn new() -> Self {
        let chat = chat::Chat::new(plugin_directory());
        let upstream = upstream::Upstream::start(Arc::clone(&chat)).ok();
        Self { chat, upstream }
    }
}

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

unsafe fn input_bytes<'a>(input: ByteSlice) -> Result<&'a [u8], String> {
    if input.len == 0 {
        return Ok(&[]);
    }
    if input.ptr.is_null() {
        return Err("null byte slice".into());
    }
    Ok(std::slice::from_raw_parts(input.ptr, input.len))
}

fn ui_asset(path: &str) -> Option<&'static (&'static str, &'static str, &'static [u8])> {
    UI_ASSETS.iter().find(|(name, _, _)| *name == path)
}

fn ui_manifest() -> Result<Value, String> {
    let manifest: Value = serde_json::from_str(UI_MANIFEST).map_err(|error| error.to_string())?;
    let assets = manifest["assets"]
        .as_array()
        .ok_or("manifest has no assets")?;
    for asset in assets.iter().filter_map(Value::as_str) {
        if ui_asset(asset).is_none() {
            return Err(format!("asset is not embedded: {asset}"));
        }
    }
    Ok(manifest)
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                output.push(TABLE[((value >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                output.push('=');
            }
        }
    }
    output
}

fn host_event(request: &Value) -> Result<Value, String> {
    let event = request.get("event").ok_or("host.event has no event")?;
    if event.get("protocol").and_then(Value::as_str) != Some(EVENTS_PROTOCOL) {
        return Err("unsupported events protocol".into());
    }
    Ok(json!({"status": "ok"}))
}

fn query_inner(state: &PluginState, request: &[u8]) -> Result<Value, String> {
    let request: Value = serde_json::from_slice(request).map_err(|error| error.to_string())?;
    match request["operation"].as_str().unwrap_or_default() {
        "host.configure" => {
            state.chat.configure(&request);
            Ok(json!({"status": "ok"}))
        }
        "host.event" => host_event(&request),
        "host.services" => Ok(state.chat.service()),
        // Asked by another node's plugin through the node's signed peer call.
        "peer.group_link" => Ok(state.chat.group_link()),
        "peer.whoami" => Ok(state.chat.whoami()),
        "ui.upstream" => state
            .upstream
            .as_ref()
            .map(upstream::Upstream::response)
            .ok_or_else(|| "upstream is unavailable".into()),
        "ui.manifest" => ui_manifest(),
        "ui.asset" => {
            let path = request["path"].as_str().ok_or("asset path is missing")?;
            let (_, content_type, bytes) = ui_asset(path).ok_or("unknown asset")?;
            Ok(json!({"content_type": content_type, "base64": encode_base64(bytes)}))
        }
        _ => Err("unknown operation".into()),
    }
}

fn buffer_from_bytes(bytes: Vec<u8>) -> ByteBuffer {
    let mut bytes = bytes.into_boxed_slice();
    let buffer = ByteBuffer {
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
    };
    std::mem::forget(bytes);
    buffer
}

unsafe extern "C" fn create(_: HostCallbacks, config: ByteSlice) -> PluginHandle {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let config = input_bytes(config)?;
        if !config.is_empty() {
            serde_json::from_slice::<Value>(config).map_err(|error| error.to_string())?;
        }
        Ok::<_, String>(Box::into_raw(Box::new(PluginState::new())).cast::<c_void>())
    }));
    match result {
        Ok(Ok(handle)) => handle,
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn ingest(handle: PluginHandle, event: ByteSlice) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if handle.is_null() {
            return false;
        }
        input_bytes(event).is_ok()
    }));
    matches!(result, Ok(true)).then_some(0).unwrap_or(-1)
}

unsafe extern "C" fn query(handle: PluginHandle, request: ByteSlice) -> ByteBuffer {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if handle.is_null() {
            return Err("plugin handle is null".to_owned());
        }
        query_inner(&*handle.cast::<PluginState>(), input_bytes(request)?)
            .and_then(|value| serde_json::to_vec(&value).map_err(|error| error.to_string()))
    }));
    match result {
        Ok(Ok(bytes)) => buffer_from_bytes(bytes),
        _ => ByteBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        },
    }
}

unsafe extern "C" fn free_buffer(buffer: ByteBuffer) {
    if !buffer.ptr.is_null() {
        let slice = std::ptr::slice_from_raw_parts_mut(buffer.ptr, buffer.len);
        drop(Box::from_raw(slice));
    }
}

unsafe extern "C" fn shutdown(handle: PluginHandle) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !handle.is_null() {
            let state = Box::from_raw(handle.cast::<PluginState>());
            state.chat.shutdown();
            drop(state);
        }
    }));
}

static API: PluginApiV1 = PluginApiV1 {
    abi_version: ABI_VERSION,
    plugin_version: VERSION.as_ptr().cast(),
    capabilities: 0,
    create: Some(create),
    ingest: Some(ingest),
    query: Some(query),
    free_buffer: Some(free_buffer),
    shutdown: Some(shutdown),
};

/// The plugin's versioned entry point.
///
/// # Safety
///
/// Returns a pointer to a static table; the host must only read it.
#[no_mangle]
pub unsafe extern "C" fn pom_ghostly_plugin_v1() -> *const PluginApiV1 {
    &API
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_exposes_the_generic_v1_boundary() {
        let api = unsafe { &*pom_ghostly_plugin_v1() };
        assert_eq!(api.abi_version, ABI_VERSION);
        assert!(api.create.is_some());
        assert!(api.query.is_some());
        assert!(api.shutdown.is_some());
    }

    #[test]
    fn buffer_encoding_uses_standard_padding() {
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
    }

    #[test]
    fn manifest_resolves_all_built_assets() {
        let manifest: Value = serde_json::from_str(UI_MANIFEST).unwrap();
        assert_eq!(manifest["plugin_code"], "ghostly");
        if ui_asset("ui/screens.js").is_some() {
            assert!(ui_manifest().is_ok());
        } else {
            assert!(ui_manifest().is_err());
        }
    }

    #[test]
    fn version_matches_the_package() {
        assert_eq!(
            std::str::from_utf8(&VERSION[..VERSION.len() - 1]).unwrap(),
            env!("CARGO_PKG_VERSION")
        );
    }
}
