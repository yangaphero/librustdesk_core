use crate::client;
use crate::client::file_trait::FileManager;
use crate::client::{Data, Interface, QualityStatus};
use crate::ui_session_interface::{io_loop, InvokeUiSession, Session};
use hbb_common::config::{self, LanPeers, LocalConfig, PeerConfig};
use hbb_common::message_proto::*;
use hbb_common::rendezvous_proto::ConnType;
use serde_json::json;
use std::sync::{LazyLock, Mutex};

// OHOS audio PCM buffer: decoded frames stored by client.rs audio handler,
// drained by ArkTS via pull_audio_frames_json()
const MAX_AUDIO_FRAMES: usize = 200;

#[derive(Clone)]
pub struct AudioPcmFrame {
    pub data_base64: String,
    pub channels: i32,
    pub sample_rate: i32,
}

pub static AUDIO_PCM_FRAMES: LazyLock<Mutex<Vec<AudioPcmFrame>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

pub fn push_audio_pcm(pcm: Vec<u8>, channels: i32, sample_rate: i32) {
    let mut buf = AUDIO_PCM_FRAMES.lock().unwrap();
    if buf.len() >= MAX_AUDIO_FRAMES {
        buf.remove(0);
    }
    use base64::Engine as _;
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&pcm);
    buf.push(AudioPcmFrame { data_base64, channels, sample_rate });
}
use std::collections::HashMap;

// Forward core log records to the host .so's hilog so failures inside the
// decode/negotiation path become visible on device (hilog tag: RustDeskCore).
extern "C" {
    fn rustdesk_ohos_native_log(level: i32, msg: *const std::os::raw::c_char);
}

struct HilogLogger;

impl hbb_common::log::Log for HilogLogger {
    fn enabled(&self, metadata: &hbb_common::log::Metadata) -> bool {
        metadata.level() <= hbb_common::log::Level::Debug
    }
    fn log(&self, record: &hbb_common::log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{}] {}:{} {}",
            record.level(),
            record.file().unwrap_or("?"),
            record.line().unwrap_or(0),
            record.args()
        );
        // hilog LogLevel: DEBUG=3, INFO=4, WARN=5, ERROR=6
        let level = match record.level() {
            hbb_common::log::Level::Error => 6,
            hbb_common::log::Level::Warn => 5,
            hbb_common::log::Level::Info => 4,
            _ => 3,
        };
        if let Ok(cstr) = std::ffi::CString::new(line) {
            unsafe { rustdesk_ohos_native_log(level, cstr.as_ptr()) };
        }
    }
    fn flush(&self) {}
}

static HILOG_INSTALL: std::sync::Once = std::sync::Once::new();

fn install_hilog_logger() {
    HILOG_INSTALL.call_once(|| {
        let _ = hbb_common::log::set_boxed_logger(Box::new(HilogLogger));
        hbb_common::log::set_max_level(hbb_common::log::LevelFilter::Debug);
    });
}
use std::os::raw::c_int;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

static CONNECT_STATE: OnceLock<Mutex<ConnectState>> = OnceLock::new();
static LOCAL_OPTIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static LATEST_VIDEO_FRAME: OnceLock<Mutex<Option<VideoFrameState>>> = OnceLock::new();
static INCOMING_SCREEN_FRAME: OnceLock<Mutex<Option<IncomingScreenFrameState>>> = OnceLock::new();
static ACTIVE_SESSION: OnceLock<Mutex<Option<Session<HarmonyHandler>>>> = OnceLock::new();
static FILE_TRANSFER_SESSION: OnceLock<Mutex<Option<Session<HarmonyHandler>>>> = OnceLock::new();
static ACTIVE_SESSION_PASSWORD: OnceLock<Mutex<String>> = OnceLock::new();
static PENDING_REMOTE_DIRECTORY: OnceLock<Mutex<Option<(String, bool)>>> = OnceLock::new();
static INCOMING_SERVICE_STARTED: OnceLock<Mutex<bool>> = OnceLock::new();
static INCOMING_SERVICE_REQUESTED: OnceLock<Mutex<bool>> = OnceLock::new();
static ACTIVE_SESSION_GENERATION: AtomicUsize = AtomicUsize::new(0);
static FILE_TRANSFER_SESSION_GENERATION: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
struct VideoFrameState {
    frame_id: u64,
    display: c_int,
    width: usize,
    height: usize,
    stride: usize,
    bytes: Vec<u8>,
    timestamp: i64,
    format: String,
}

#[derive(Clone, Debug)]
struct IncomingScreenFrameState {
    frame_id: u64,
    width: usize,
    height: usize,
    stride: usize,
    data_length: usize,
    bytes: Vec<u8>,
    timestamp: i64,
    format: String,
    frames_seen: u64,
}

#[derive(Clone, Debug, Default)]
struct ConnectState {
    session_stage: String,
    active_peer_id: String,
    status_summary: String,
    detail_message: String,
    last_error: String,
    events: Vec<String>,
}

fn connect_state() -> &'static Mutex<ConnectState> {
    CONNECT_STATE.get_or_init(|| Mutex::new(ConnectState::default()))
}

fn local_options() -> &'static Mutex<HashMap<String, String>> {
    LOCAL_OPTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

const HARMONY_SESSION_DEFAULT_OPTIONS: &[&str] = &[
    "image-quality",
    "custom-image-quality",
    "custom-fps",
    "keyboard-mode",
    "codec-preference",
    "view-only",
    "show-remote-cursor",
    "show-quality-monitor",
    "disable-audio",
    "disable-clipboard",
    "lock-after-session-end",
    "reverse-scroll",
    "swap-left-right-mouse",
    "terminal-persistent",
];

const WIRE_TOGGLE_OPTIONS: &[&str] = &[
    "view-only",
    "show-remote-cursor",
    "show-quality-monitor",
    "disable-audio",
    "disable-clipboard",
    "lock-after-session-end",
    "terminal-persistent",
];

fn is_harmony_persistent_option(key: &str) -> bool {
    HARMONY_SESSION_DEFAULT_OPTIONS.contains(&key)
        || key == "display-scale-mode"
        || key == "custom-zoom-percent"
        || key == "direct-server"
        || key == "direct-access-port"
}

fn latest_video_frame() -> &'static Mutex<Option<VideoFrameState>> {
    LATEST_VIDEO_FRAME.get_or_init(|| Mutex::new(None))
}

fn incoming_screen_frame() -> &'static Mutex<Option<IncomingScreenFrameState>> {
    INCOMING_SCREEN_FRAME.get_or_init(|| Mutex::new(None))
}

fn active_session() -> &'static Mutex<Option<Session<HarmonyHandler>>> {
    ACTIVE_SESSION.get_or_init(|| Mutex::new(None))
}

fn file_transfer_session() -> &'static Mutex<Option<Session<HarmonyHandler>>> {
    FILE_TRANSFER_SESSION.get_or_init(|| Mutex::new(None))
}

fn active_session_password() -> &'static Mutex<String> {
    ACTIVE_SESSION_PASSWORD.get_or_init(|| Mutex::new(String::new()))
}

fn pending_remote_directory() -> &'static Mutex<Option<(String, bool)>> {
    PENDING_REMOTE_DIRECTORY.get_or_init(|| Mutex::new(None))
}

fn incoming_service_started() -> &'static Mutex<bool> {
    INCOMING_SERVICE_STARTED.get_or_init(|| Mutex::new(false))
}

pub fn set_incoming_service_started(started: bool) {
    *incoming_service_started().lock().unwrap() = started;
    if started {
        queue_event(
            "incoming-service-ready",
            "OHOS incoming service is now ready (video source active)",
            "",
        );
    }
}

fn incoming_service_requested() -> &'static Mutex<bool> {
    INCOMING_SERVICE_REQUESTED.get_or_init(|| Mutex::new(false))
}

fn get_local_option(key: &str) -> String {
    main_get_local_option(key)
}

fn set_local_option(key: &str, value: &str) {
    main_set_local_option(key, value)
}

pub fn get_session_stage() -> String {
    connect_state().lock().unwrap().session_stage.clone()
}

pub fn get_active_peer_id() -> String {
    connect_state().lock().unwrap().active_peer_id.clone()
}

pub fn get_connect_status_summary() -> String {
    connect_state().lock().unwrap().status_summary.clone()
}

pub fn get_connect_detail_message() -> String {
    connect_state().lock().unwrap().detail_message.clone()
}

pub fn get_connect_last_error() -> String {
    connect_state().lock().unwrap().last_error.clone()
}

pub fn drain_connect_events_json() -> String {
    let mut guard = connect_state().lock().unwrap();
    let events: Vec<String> = guard.events.drain(..).collect();
    format!("[{}]", events.join(","))
}

fn clear_connect_events() {
    connect_state().lock().unwrap().events.clear();
}

fn update_connect_state(stage: &str, peer_id: &str, summary: &str, detail: &str, error: &str) {
    let mut guard = connect_state().lock().unwrap();
    guard.session_stage = stage.to_owned();
    guard.active_peer_id = peer_id.to_owned();
    guard.status_summary = summary.to_owned();
    guard.detail_message = detail.to_owned();
    guard.last_error = error.to_owned();
}

pub(crate) fn queue_event(kind: &str, detail: &str, peer_id: &str) {
    let event = format!(
        "{{\"kind\":\"{}\",\"detail\":\"{}\",\"peerId\":\"{}\",\"timestamp\":{}}}",
        escape_json(kind),
        escape_json(detail),
        escape_json(peer_id),
        current_timestamp_millis()
    );
    let mut guard = connect_state().lock().unwrap();
    guard.events.push(event);
    if guard.events.len() > 50 {
        let excess = guard.events.len() - 50;
        guard.events.drain(0..excess);
    }
}

fn current_timestamp_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_millis() as i64,
        Err(_) => 0,
    }
}

fn next_bridge_job_id() -> i32 {
    (current_timestamp_millis() & 0x7fff_ffff) as i32
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Returns a JSON snapshot of the core state for the given server.
pub fn get_core_snapshot_json(server: &str) -> String {
    let incoming_ready = *incoming_service_started().lock().unwrap();
    let incoming_requested = *incoming_service_requested().lock().unwrap();
    let incoming_frame = incoming_screen_frame().lock().unwrap().clone();
    let incoming_frame_payload_ready = incoming_frame
        .as_ref()
        .map(|frame| !frame.bytes.is_empty() && frame.width > 0 && frame.height > 0)
        .unwrap_or(false);
    let status_summary = get_connect_status_summary();
    let detail_message = get_connect_detail_message();
    let last_error = get_connect_last_error();
    json!({
        "adapter": "official-native",
        "coreReady": true,
        "incomingReady": incoming_ready,
        "displayId": config::Config::get_id(),
        "fingerprint": crate::common::pk_to_fingerprint(config::Config::get_key_pair().1),
        "directAddress": "",
        "server": server,
        "captureRequired": incoming_requested && !incoming_ready,
        "incomingFramePayloadReady": incoming_frame_payload_ready,
        "incomingFrameId": incoming_frame.as_ref().map(|frame| frame.frame_id).unwrap_or(0),
        "incomingFrameBytes": incoming_frame.as_ref().map(|frame| frame.data_length).unwrap_or(0),
        "incomingFramesSeen": incoming_frame.as_ref().map(|frame| frame.frames_seen).unwrap_or(0),
        "statusSummary": if !status_summary.trim().is_empty() {
            status_summary
        } else if incoming_ready {
            "Incoming service ready".to_owned()
        } else if incoming_requested {
            "Incoming service starting".to_owned()
        } else {
            "Official Harmony bridge ready".to_owned()
        },
        "detailMessage": if !detail_message.trim().is_empty() {
            detail_message
        } else if incoming_ready {
            "OHOS incoming service is active. Video source is encoding frames for connected peers.".to_owned()
        } else if incoming_requested && incoming_frame_payload_ready {
            "Harmony native screen frames are cached in core. Desktop server subscription is still disabled on Harmony.".to_owned()
        } else if incoming_requested {
            "Harmony incoming service requested. Waiting for native screen recording to provide the first live frame.".to_owned()
        } else {
            "Official Harmony bridge is initialized.".to_owned()
        },
        "lastError": last_error,
        "sessionStage": get_session_stage(),
        "activePeerId": get_active_peer_id(),
    })
    .to_string()
}

/// Initializes the runtime with the given app directory and custom client config.
/// Returns a JSON string with initialization result.
pub fn initialize_runtime(app_dir: &str, _custom_client_config: &str) -> String {
    install_hilog_logger();
    set_local_option("app_dir", app_dir);
    if !app_dir.trim().is_empty() {
        *config::APP_DIR.write().unwrap() = app_dir.trim().to_owned();
    }
    let id = config::Config::get_id();
    let _key_pair = config::Config::get_key_pair();
    let fp = crate::common::pk_to_fingerprint(config::Config::get_key_pair().1);
    json!({
        "id": id,
        "fingerprint": fp,
    })
    .to_string()
}

/// Pulls pending session events as a JSON string.
pub fn pull_session_events_json() -> String {
    drain_connect_events_json()
}

/// Pulls pending audio frames as a JSON string.
/// Drains internal buffer; each frame has base64 16-bit LE PCM.
pub fn pull_audio_frames_json() -> String {
    let mut buf = AUDIO_PCM_FRAMES.lock().unwrap();
    if buf.is_empty() {
        return "[]".to_owned();
    }
    let frames: Vec<_> = buf
        .drain(..)
        .map(|f| {
            json!({
                "dataBase64": f.data_base64,
                "channels": f.channels,
                "sampleRate": f.sample_rate,
                "format": "S16LE",
            })
        })
        .collect();
    json!(frames).to_string()
}

/// Returns the latest video frame metadata as JSON since the given frame ID.
pub fn get_latest_video_frame_metadata_json(since_frame_id: u64) -> String {
    let guard = latest_video_frame().lock().unwrap();
    let Some(frame) = guard.as_ref() else {
        return "{}".to_owned();
    };
    if frame.frame_id <= since_frame_id {
        return "{}".to_owned();
    }
    json!({
        "frameId": frame.frame_id,
        "display": frame.display,
        "width": frame.width,
        "height": frame.height,
        "stride": frame.stride,
        "bytes": frame.bytes.len(),
        "timestamp": frame.timestamp,
        "format": frame.format,
    })
    .to_string()
}

/// Copies the latest video frame data into the provided buffer.
/// Returns the number of bytes written, or 0 on failure.
pub fn copy_latest_video_frame(frame_id: u64, buffer: &mut [u8]) -> c_int {
    let guard = latest_video_frame().lock().unwrap();
    let Some(frame) = guard.as_ref() else {
        return 0;
    };
    if frame.frame_id != frame_id || buffer.len() < frame.bytes.len() {
        return 0;
    }
    buffer[..frame.bytes.len()].copy_from_slice(&frame.bytes);
    frame.bytes.len() as c_int
}

/// Returns the latest incoming screen frame metadata as JSON since the given frame ID.
pub fn get_incoming_screen_frame_metadata_json(since_frame_id: u64) -> String {
    let guard = incoming_screen_frame().lock().unwrap();
    let Some(frame) = guard.as_ref() else {
        return "{}".to_owned();
    };
    if frame.frame_id <= since_frame_id {
        return "{}".to_owned();
    }
    json!({
        "frameId": frame.frame_id,
        "width": frame.width,
        "height": frame.height,
        "stride": frame.stride,
        "bytes": frame.data_length,
        "payloadBytes": frame.bytes.len(),
        "hasPayload": !frame.bytes.is_empty(),
        "timestamp": frame.timestamp,
        "format": frame.format,
        "framesSeen": frame.frames_seen,
    })
    .to_string()
}

/// Copies the latest incoming screen frame data into the provided buffer.
/// Returns the number of bytes written, or 0 on failure.
pub fn copy_incoming_screen_frame(frame_id: u64, buffer: &mut [u8]) -> c_int {
    let guard = incoming_screen_frame().lock().unwrap();
    let Some(frame) = guard.as_ref() else {
        return 0;
    };
    if frame.frame_id != frame_id || frame.bytes.is_empty() || buffer.len() < frame.bytes.len() {
        return 0;
    }
    buffer[..frame.bytes.len()].copy_from_slice(&frame.bytes);
    frame.bytes.len() as c_int
}

/// Clears the latest incoming screen frame cache.
pub fn clear_incoming_screen_frame() {
    *incoming_screen_frame().lock().unwrap() = None;
    #[cfg(target_env = "ohos")]
    scrap::clear_ohos_incoming_frame();
}

/// Updates the latest incoming screen frame from the Harmony native capture path.
/// Returns true when the frame payload was accepted by the core bridge cache.
pub fn update_incoming_screen_frame(
    width: c_int,
    height: c_int,
    stride: c_int,
    timestamp: i64,
    format: &str,
    data: &[u8],
) -> bool {
    if width <= 0 || height <= 0 || stride <= 0 || data.is_empty() {
        return false;
    }
    let width = width as usize;
    let height = height as usize;
    let stride = stride as usize;
    let min_len = stride.saturating_mul(height);
    if min_len == 0 || data.len() < min_len {
        return false;
    }
    let normalized_format = if format.trim().is_empty() {
        "RGBA".to_owned()
    } else {
        format.trim().to_owned()
    };
    let mut guard = incoming_screen_frame().lock().unwrap();
    let first_frame = guard.is_none();
    let (frame_id, frames_seen) = guard
        .as_ref()
        .map(|frame| {
            (
                frame.frame_id.saturating_add(1),
                frame.frames_seen.saturating_add(1),
            )
        })
        .unwrap_or((1, 1));
    let next_frame = IncomingScreenFrameState {
        frame_id,
        width,
        height,
        stride,
        data_length: min_len,
        bytes: data[..min_len].to_vec(),
        timestamp,
        format: normalized_format,
        frames_seen,
    };
    #[cfg(target_env = "ohos")]
    if !scrap::update_ohos_incoming_frame(
        frame_id,
        width,
        height,
        stride,
        &next_frame.format,
        &next_frame.bytes,
    ) {
        return false;
    }
    *guard = Some(next_frame);
    drop(guard);
    if first_frame {
        queue_event(
            "incoming-video-source-active",
            &format!("width={width};height={height};stride={stride};bytes={min_len}"),
            "",
        );
    }
    true
}

/// Refreshes the session video for the given display.
/// Returns true if the refresh was successful.
pub fn refresh_session_video(display: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    let display = display.max(0);
    session.request_init_msgs(display as usize);
    session.refresh_video(display);
    queue_event(
        "video-refresh-requested",
        &format!("display={display}"),
        &get_active_peer_id(),
    );
    true
}

/// Advances to the next RGBA frame for the given display index.
pub fn harmony_next_rgba(display: usize) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.ui_handler.next_rgba(display);
}

/// Enables or disables the incoming service with the given server configuration.
/// Returns a JSON string with the result.
pub fn main_start_service(
    enabled: bool,
    server: &str,
    relay_server: &str,
    api_server: &str,
    key: &str,
) -> String {
    apply_server_options(server, relay_server, api_server, key);

    if enabled {
        if config::Config::get_option("direct-server").trim().is_empty() {
            config::Config::set_option("direct-server".to_owned(), "Y".to_owned());
        }
        config::Config::set_option("stop-service".to_owned(), "".to_owned());
        *incoming_service_started().lock().unwrap() = false;
        *incoming_service_requested().lock().unwrap() = true;
        clear_incoming_screen_frame();
        std::thread::spawn(|| {
            let rt = hbb_common::tokio::runtime::Runtime::new().unwrap();
            rt.block_on(crate::start_server(true, false));
        });
        let detail = "Harmony incoming service starting. Server thread launched, connecting to signaling servers.";
        queue_event("incoming-service-requested", detail, "");
        json!({
            "adapter": "official-native",
            "coreReady": true,
            "incomingReady": false,
            "displayId": config::Config::get_id(),
            "fingerprint": crate::common::pk_to_fingerprint(config::Config::get_key_pair().1),
            "directAddress": "",
            "server": server,
            "captureRequired": true,
            "incomingFramePayloadReady": false,
            "incomingFrameId": 0,
            "incomingFrameBytes": 0,
            "incomingFramesSeen": 0,
            "statusSummary": "Incoming service starting",
            "detailMessage": detail,
            "lastError": "",
            "sessionStage": get_session_stage(),
            "activePeerId": get_active_peer_id(),
        })
        .to_string()
    } else {
        config::Config::set_option("direct-server".to_owned(), "N".to_owned());
        config::Config::set_option("stop-service".to_owned(), "Y".to_owned());
        crate::common::set_server_running(false);
        crate::RendezvousMediator::restart();
        *incoming_service_started().lock().unwrap() = false;
        *incoming_service_requested().lock().unwrap() = false;
        clear_incoming_screen_frame();
        json!({
            "adapter": "official-native",
            "coreReady": true,
            "incomingReady": false,
            "displayId": config::Config::get_id(),
            "fingerprint": crate::common::pk_to_fingerprint(config::Config::get_key_pair().1),
            "directAddress": "",
            "server": server,
            "captureRequired": false,
            "incomingFramePayloadReady": false,
            "statusSummary": "Incoming service stopped",
            "detailMessage": "Incoming service stop has been requested by the Harmony bridge.",
            "lastError": "",
            "sessionStage": get_session_stage(),
            "activePeerId": get_active_peer_id(),
        })
        .to_string()
    }
}

/// Bootstraps a core snapshot with the given connection parameters.
/// Returns a JSON string with the result.
pub fn bootstrap_core_snapshot(
    _display_id: &str,
    _fingerprint: &str,
    _direct_address: &str,
    _server: &str,
) -> String {
    "{}".to_owned()
}

/// Sends mouse input with the given mask and coordinates.
/// Returns true if the input was sent successfully.
pub fn session_send_mouse(mask: c_int, x: c_int, y: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event("mouse-input", "failed=no-active-session", "");
        return false;
    };
    session.send_mouse(mask as i32, x as i32, y as i32, false, false, false, false);
    queue_event(
        "mouse-input",
        &format!("mask={mask};x={x};y={y}"),
        &get_active_peer_id(),
    );
    true
}

/// Sends keyboard input with the given key code, press state, and modifiers.
/// Returns true if the input was sent successfully.
pub fn session_input_key(key_code: c_int, is_pressed: bool, modifiers: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "keyboard-input",
            &format!("failed=no-active-session;key={key_code};down={is_pressed}"),
            "",
        );
        return false;
    };
    let Some(name) = key_code_to_official_key_name(key_code) else {
        queue_event(
            "keyboard-input",
            &format!("failed=unsupported-key;key={key_code};down={is_pressed}"),
            &get_active_peer_id(),
        );
        return false;
    };
    let ctrl = modifiers & 1 != 0;
    let alt = modifiers & 2 != 0;
    let shift = modifiers & 4 != 0;
    let command = modifiers & 8 != 0;
    session.input_key(&name, is_pressed, false, alt, ctrl, shift, command);
    queue_event(
        "keyboard-input",
        &format!("key={key_code};name={name};down={is_pressed};modifiers={modifiers}"),
        &get_active_peer_id(),
    );
    true
}

fn key_code_to_official_key_name(key_code: c_int) -> Option<String> {
    let name = match key_code {
        8 => "VK_BACK",
        9 => "VK_TAB",
        13 => "VK_RETURN",
        16 => "VK_SHIFT",
        17 => "VK_CONTROL",
        18 => "VK_MENU",
        20 => "VK_CAPITAL",
        27 => "VK_ESCAPE",
        32 => "VK_SPACE",
        33 => "VK_PRIOR",
        34 => "VK_NEXT",
        35 => "VK_END",
        36 => "VK_HOME",
        37 => "VK_LEFT",
        38 => "VK_UP",
        39 => "VK_RIGHT",
        40 => "VK_DOWN",
        44 => "VK_SNAPSHOT",
        45 => "VK_INSERT",
        46 => "VK_DELETE",
        91 => "Meta",
        92 => "RWin",
        93 => "Apps",
        112 => "VK_F1",
        113 => "VK_F2",
        114 => "VK_F3",
        115 => "VK_F4",
        116 => "VK_F5",
        117 => "VK_F6",
        118 => "VK_F7",
        119 => "VK_F8",
        120 => "VK_F9",
        121 => "VK_F10",
        122 => "VK_F11",
        123 => "VK_F12",
        _ => "",
    };
    if !name.is_empty() {
        return Some(name.to_owned());
    }
    if (48..=57).contains(&key_code) {
        return Some(format!("VK_{}", (key_code as u8) as char));
    }
    if (65..=90).contains(&key_code) {
        return Some(format!("VK_{}", (key_code as u8) as char));
    }
    if (32..=126).contains(&key_code) {
        return Some((key_code as u8 as char).to_string());
    }
    None
}

/// Sends the official RustDesk Ctrl+Alt+Del control key event.
/// Returns true if an active session accepted the event.
pub fn session_ctrl_alt_del() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "keyboard-input",
            "failed=no-active-session; command=ctrl-alt-del",
            "",
        );
        return false;
    };
    session.ctrl_alt_del();
    queue_event(
        "keyboard-input",
        "command=ctrl-alt-del",
        &get_active_peer_id(),
    );
    true
}

/// Sends clipboard data with the given content and timestamp.
/// Returns true if the data was sent successfully.
pub fn send_clipboard_data(content: &str, _timestamp: i64) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    use hbb_common::message_proto::{Clipboard, ClipboardFormat, Message};
    let clipboard = Clipboard {
        compress: false,
        content: bytes::Bytes::from(content.to_owned()),
        format: ClipboardFormat::Text.into(),
        ..Default::default()
    };
    let mut msg = Message::new();
    msg.set_clipboard(clipboard);
    session.send(Data::Message(msg));
    true
}

/// Sends video frame metadata.
/// Returns true if the metadata was sent successfully.
pub fn send_video_frame_metadata(
    codec: c_int,
    width: c_int,
    height: c_int,
    timestamp: i64,
    key_frame: bool,
    data_length: c_int,
) -> bool {
    if width <= 0 || height <= 0 || data_length <= 0 {
        return false;
    }
    queue_event(
        "incoming-video-frame-metadata",
        &format!(
            "codec={codec};width={width};height={height};timestamp={timestamp};keyFrame={};bytes={data_length}",
            if key_frame { 1 } else { 0 }
        ),
        "",
    );
    true
}

/// Sends audio frame metadata.
/// Returns true if the metadata was sent successfully.
pub fn send_audio_frame_metadata(
    _codec: c_int,
    _sample_rate: c_int,
    _channels: c_int,
    _timestamp: i64,
    _data_length: c_int,
) -> bool {
    false
}

/// Sends a chat message with the given content.
/// Returns true if the message was sent successfully.
pub fn session_send_chat(content: &str) -> bool {
    let normalized = content.trim();
    if normalized.is_empty() {
        queue_event("chat-error", "failed=empty-content", &get_active_peer_id());
        return false;
    }
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event("chat-error", "failed=no-active-session", "");
        return false;
    };
    session.send_chat(normalized.to_owned());
    queue_event("chat-sent", normalized, &get_active_peer_id());
    true
}

/// Sends a file transfer request.
/// Returns true if the request was sent successfully.
pub fn send_file_transfer_request(
    task_id: &str,
    file_name: &str,
    total_bytes: i64,
    direction: &str,
) -> bool {
    queue_event(
        "file-transfer-request",
        &json!({
            "taskId": task_id,
            "fileName": file_name,
            "totalBytes": total_bytes,
            "direction": direction
        })
        .to_string(),
        &get_active_peer_id(),
    );
    active_session().lock().unwrap().is_some()
}

/// Connects to a peer with the given connection parameters.
pub fn session_start(
    peer_id: &str,
    password: &str,
    server: &str,
    relay_server: &str,
    api_server: &str,
    key: &str,
) {
    close_file_transfer_session();
    let generation = ACTIVE_SESSION_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    *active_session_password().lock().unwrap() = password.to_owned();
    clear_connect_events();
    *latest_video_frame().lock().unwrap() = None;
    apply_server_options(server, relay_server, api_server, key);

    // Harmony keeps remembered passwords in the application preference store. Clear
    // the upstream peer cache so one-time credentials cannot leak into a new request.
    main_forget_password(peer_id);

    update_connect_state(
        "connecting",
        peer_id,
        &format!("Connecting to {peer_id}"),
        "Connecting to remote peer",
        "",
    );
    queue_event(
        "connecting",
        &format!(
            "Connecting to {}; server={}; relay={}; api={}",
            peer_id, server, relay_server, api_server
        ),
        peer_id,
    );

    let session = Session::<HarmonyHandler> {
        password: password.to_owned(),
        server_keyboard_enabled: Arc::new(RwLock::new(true)),
        server_file_transfer_enabled: Arc::new(RwLock::new(true)),
        server_clipboard_enabled: Arc::new(RwLock::new(true)),
        reconnect_count: Arc::new(AtomicUsize::new(0)),
        ui_handler: HarmonyHandler {
            generation,
            file_transfer: false,
        },
        ..Default::default()
    };
    session.lc.write().unwrap().initialize(
        peer_id.to_owned(),
        ConnType::DEFAULT_CONN,
        None,
        false,
        None,
        None,
        None,
    );
    for option_key in HARMONY_SESSION_DEFAULT_OPTIONS {
        let option_value = get_local_option(option_key);
        if option_value.is_empty() {
            continue;
        }
        if WIRE_TOGGLE_OPTIONS.contains(option_key) {
            set_session_toggle(&session, option_key, option_is_enabled(&option_value));
        } else if *option_key == "image-quality" {
            let normalized = normalize_image_quality(&option_value);
            if is_supported_image_quality(normalized) {
                session.save_image_quality(normalized.to_owned());
            }
        } else if *option_key == "custom-image-quality" {
            if let Ok(value) = option_value.parse::<i32>() {
                session.save_custom_image_quality(value);
            }
        } else if *option_key == "custom-fps" {
            if let Ok(value) = option_value.parse::<i32>() {
                session.set_custom_fps(value);
            }
        } else if *option_key == "keyboard-mode" {
            session.save_keyboard_mode(option_value);
        } else {
            session.set_option((*option_key).to_owned(), option_value);
        }
    }
    let round = session.connection_round_state.lock().unwrap().new_round();
    *active_session().lock().unwrap() = Some(session.clone());
    std::thread::spawn(move || {
        io_loop(session, round);
    });
}

fn close_file_transfer_session() {
    FILE_TRANSFER_SESSION_GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Some(session) = file_transfer_session().lock().unwrap().as_ref() {
        session.send(Data::Close);
    }
    *file_transfer_session().lock().unwrap() = None;
    *pending_remote_directory().lock().unwrap() = None;
}

fn start_file_transfer_session(path: &str, include_hidden: bool) -> bool {
    let peer_id = get_active_peer_id();
    if peer_id.trim().is_empty() || active_session().lock().unwrap().is_none() {
        queue_event(
            "file-transfer-error",
            "No active remote-control session",
            &peer_id,
        );
        return false;
    }

    *pending_remote_directory().lock().unwrap() = Some((path.trim().to_owned(), include_hidden));
    if let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() {
        session.read_remote_dir(path.trim().to_owned(), include_hidden);
        return true;
    }

    let generation = FILE_TRANSFER_SESSION_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let password = active_session_password().lock().unwrap().clone();
    let session = Session::<HarmonyHandler> {
        password,
        server_keyboard_enabled: Arc::new(RwLock::new(false)),
        server_file_transfer_enabled: Arc::new(RwLock::new(true)),
        server_clipboard_enabled: Arc::new(RwLock::new(false)),
        reconnect_count: Arc::new(AtomicUsize::new(0)),
        ui_handler: HarmonyHandler {
            generation,
            file_transfer: true,
        },
        ..Default::default()
    };
    session.lc.write().unwrap().initialize(
        peer_id.clone(),
        ConnType::FILE_TRANSFER,
        None,
        false,
        None,
        None,
        None,
    );
    let round = session.connection_round_state.lock().unwrap().new_round();
    *file_transfer_session().lock().unwrap() = Some(session.clone());
    queue_event(
        "file-transfer-connecting",
        &json!({"path":path.trim(),"includeHidden":include_hidden}).to_string(),
        &peer_id,
    );
    std::thread::spawn(move || {
        io_loop(session, round);
    });
    true
}

fn apply_server_options(server: &str, relay_server: &str, api_server: &str, key: &str) {
    config::Config::set_option(
        "custom-rendezvous-server".to_owned(),
        server.trim().to_owned(),
    );
    config::Config::set_option("relay-server".to_owned(), relay_server.trim().to_owned());
    config::Config::set_option("api-server".to_owned(), api_server.trim().to_owned());
    config::Config::set_option("key".to_owned(), key.trim().to_owned());
}

/// Performs account authentication with the given parameters.
pub fn main_account_auth(
    _op: &str,
    _remember_me: bool,
    _server: &str,
    _relay_server: &str,
    _api_server: &str,
) {
}

/// Cancels an in-progress account authentication.
pub fn main_account_auth_cancel() {}

/// Returns the account authentication result as a JSON string.
pub fn main_account_auth_result() -> String {
    "{}".to_owned()
}

/// Returns the value of a local option by key.
pub fn main_get_local_option(key: &str) -> String {
    if key == "access_token" || key == "user_info" || key == "lang" {
        let value = LocalConfig::get_option(key);
        if !value.trim().is_empty() {
            return value;
        }
    }
    let value = local_options()
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .unwrap_or_default();
    if value.is_empty() && is_harmony_persistent_option(key) {
        return config::Config::get_option(key);
    }
    value
}

/// Returns the value of a saved peer option by key.
pub fn main_get_peer_option(peer_id: &str, key: &str) -> String {
    let config = PeerConfig::load(peer_id);
    config.options.get(key).cloned().unwrap_or_default()
}

/// Returns saved peer display information as JSON.
pub fn get_peer_info(peer_id: &str) -> String {
    let config = PeerConfig::load(peer_id);
    let info = &config.info;
    let alias = config
        .options
        .get("alias")
        .map(String::as_str)
        .unwrap_or_default();

    json!({
        "hostname": info.hostname,
        "username": info.username,
        "platform": info.platform,
        "alias": alias,
    })
    .to_string()
}

fn read_cached_peer_info(peer_id: &str) -> (String, String, String, String) {
    let config = PeerConfig::load(peer_id);
    let info = config.info;
    let alias = config.options.get("alias").cloned().unwrap_or_default();
    (info.hostname, info.username, info.platform, alias)
}

fn peer_info_detail(peer_id: &str) -> String {
    let (hostname, username, platform, alias) = read_cached_peer_info(peer_id);
    let resolved_platform = if platform.trim().is_empty() {
        "RustDesk".to_owned()
    } else {
        platform
    };
    format!(
        "Session connected; hostname={}; username={}; platform={}; alias={}",
        hostname, username, resolved_platform, alias
    )
}

#[derive(Clone, Default)]
struct HarmonyHandler {
    generation: usize,
    file_transfer: bool,
}

impl HarmonyHandler {
    fn is_current(&self) -> bool {
        let current_generation = if self.file_transfer {
            FILE_TRANSFER_SESSION_GENERATION.load(Ordering::SeqCst)
        } else {
            ACTIVE_SESSION_GENERATION.load(Ordering::SeqCst)
        };
        self.generation != 0 && self.generation == current_generation
    }

    fn queue_event(&self, kind: &str, detail: &str, peer_id: &str) {
        if self.is_current() {
            queue_event(kind, detail, peer_id);
        }
    }

    fn session(&self) -> Option<Session<HarmonyHandler>> {
        if self.file_transfer {
            file_transfer_session().lock().unwrap().as_ref().cloned()
        } else {
            active_session().lock().unwrap().as_ref().cloned()
        }
    }
}

impl InvokeUiSession for HarmonyHandler {
    fn set_cursor_data(&self, cd: CursorData) {
        if self.file_transfer {
            return;
        }
        let mut colors = hbb_common::compress::decompress(&cd.colors);
        if colors.len() >= 4 && colors.iter().all(|value| *value == 0) {
            colors[3] = 1;
        }
        self.queue_event(
            "cursor_data",
            &json!({
                "id": cd.id.to_string(),
                "hotx": cd.hotx,
                "hoty": cd.hoty,
                "width": cd.width,
                "height": cd.height,
                "colors": colors,
            })
            .to_string(),
            &get_active_peer_id(),
        );
    }

    fn set_cursor_id(&self, id: String) {
        if self.file_transfer {
            return;
        }
        self.queue_event("cursor_id", &format!("id={id}"), &get_active_peer_id());
    }

    fn set_cursor_position(&self, cp: CursorPosition) {
        if self.file_transfer {
            return;
        }
        self.queue_event(
            "cursor_position",
            &format!("x={};y={}", cp.x, cp.y),
            &get_active_peer_id(),
        );
    }

    fn set_display(&self, x: i32, y: i32, w: i32, h: i32, cursor_embedded: bool, scale: f64) {
        if self.file_transfer {
            return;
        }
        self.queue_event(
            "display",
            &format!(
                "x={x};y={y};width={w};height={h};cursorEmbedded={cursor_embedded};scale={}",
                if scale > 0.0 { scale } else { 1.0 }
            ),
            &get_active_peer_id(),
        );
    }

    fn switch_display(&self, display: &SwitchDisplay) {
        if self.file_transfer {
            return;
        }
        let resolutions: Vec<serde_json::Value> = display
            .resolutions
            .resolutions
            .iter()
            .map(|resolution| {
                json!({
                    "width": resolution.width,
                    "height": resolution.height,
                })
            })
            .collect();
        self.queue_event(
            "switch-display",
            &format!(
                "display={};x={};y={};width={};height={};cursorEmbedded={};originalWidth={};originalHeight={};resolutions={}",
                display.display,
                display.x,
                display.y,
                display.width,
                display.height,
                display.cursor_embedded,
                display.original_resolution.width,
                display.original_resolution.height,
                serde_json::to_string(&resolutions).unwrap_or_else(|_| "[]".to_owned()),
            ),
            &get_active_peer_id(),
        );
        self.set_display(
            display.x,
            display.y,
            display.width,
            display.height,
            display.cursor_embedded,
            1.0,
        );
    }

    fn set_peer_info(&self, peer_info: &PeerInfo) {
        if !self.is_current() {
            return;
        }
        let peer_id = get_active_peer_id();
        if let Some(session) = self.session() {
            session.lc.write().unwrap().handle_peer_info(peer_info);
        }
        if self.file_transfer {
            self.queue_event(
                "file-transfer-peer-info",
                &format!(
                    "hostname={};username={};platform={};version={}",
                    peer_info.hostname, peer_info.username, peer_info.platform, peer_info.version
                ),
                &peer_id,
            );
            return;
        }
        let detail = format!(
            "hostname={}; username={}; platform={}; version={}",
            peer_info.hostname, peer_info.username, peer_info.platform, peer_info.version
        );
        self.queue_event("peer-info", &detail, &peer_id);
    }

    fn set_displays(&self, displays: &Vec<DisplayInfo>) {
        if self.file_transfer {
            return;
        }
        let payload: Vec<serde_json::Value> = displays
            .iter()
            .map(|display| {
                let (original_width, original_height) = display
                    .original_resolution
                    .as_ref()
                    .map(|resolution| (resolution.width, resolution.height))
                    .unwrap_or((0, 0));
                json!({
                    "x": display.x,
                    "y": display.y,
                    "width": display.width,
                    "height": display.height,
                    "name": display.name,
                    "online": display.online,
                    "cursorEmbedded": display.cursor_embedded,
                    "originalWidth": original_width,
                    "originalHeight": original_height,
                    "scale": display.scale,
                })
            })
            .collect();
        self.queue_event(
            "displays",
            &serde_json::to_string(&payload).unwrap_or_else(|_| "[]".to_owned()),
            &get_active_peer_id(),
        );
    }
    fn set_platform_additions(&self, _data: &str) {}

    fn on_connected(&self, conn_type: ConnType) {
        if !self.is_current() {
            return;
        }
        let peer_id = get_active_peer_id();
        if self.file_transfer {
            self.queue_event(
                "file-transfer-connected",
                &format!("connType={conn_type:?}"),
                &peer_id,
            );
            return;
        }
        let detail = format!("Connected; connType={conn_type:?}");
        update_connect_state("connected", &peer_id, "Connected", &detail, "");
        self.queue_event("session-connected", &detail, &peer_id);
        if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
            session.request_init_msgs(0);
            session.refresh_video(0);
        }
    }

    fn update_privacy_mode(&self) {
        if self.file_transfer {
            return;
        }
        let enabled = active_session()
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.get_toggle_option("privacy-mode".to_owned()))
            .unwrap_or(false);
        self.queue_event(
            "privacy-mode-state",
            if enabled { "on=true" } else { "on=false" },
            &get_active_peer_id(),
        );
    }
    fn set_permission(&self, _name: &str, _value: bool) {}

    fn close_success(&self) {
        if !self.is_current() {
            return;
        }
        let peer_id = get_active_peer_id();
        if self.file_transfer {
            self.queue_event("file-transfer-ready", "ready=true", &peer_id);
            if let Some((path, include_hidden)) = pending_remote_directory().lock().unwrap().take()
            {
                if !path.is_empty() {
                    if let Some(session) = self.session() {
                        session.read_remote_dir(path, include_hidden);
                    }
                }
            }
            return;
        }
        update_connect_state(
            "connected",
            &peer_id,
            "Connected",
            "Connection handshake completed",
            "",
        );
        self.queue_event(
            "connection-ready",
            "Connection handshake completed",
            &peer_id,
        );
    }

    fn update_quality_status(&self, qs: QualityStatus) {
        if !self.is_current() || self.file_transfer {
            return;
        }
        let detail = json!({
            "speed": qs.speed,
            "fps": qs.fps,
            "delay": qs.delay,
            "target_bitrate": qs.target_bitrate,
            "codec_format": qs.codec_format.map(|it| it.to_string()),
            "chroma": qs.chroma,
        })
        .to_string();
        self.queue_event("quality-status", &detail, &get_active_peer_id());
    }

    fn set_connection_type(&self, is_secured: bool, direct: bool, stream_type: &str) {
        if !self.is_current() {
            return;
        }
        let peer_id = get_active_peer_id();
        if self.file_transfer {
            self.queue_event(
                "file-transfer-connection-type",
                &format!("secured={is_secured};direct={direct};stream={stream_type}"),
                &peer_id,
            );
            return;
        }
        self.queue_event(
            "connection-type",
            &format!("secured={is_secured}; direct={direct}; stream={stream_type}"),
            &peer_id,
        );
    }

    fn set_fingerprint(&self, fingerprint: String) {
        self.queue_event("fingerprint", &fingerprint, &get_active_peer_id());
    }

    fn job_error(&self, id: i32, err: String, file_num: i32) {
        self.queue_event(
            "job-error",
            &json!({"id":id,"error":err,"file_num":file_num}).to_string(),
            &get_active_peer_id(),
        );
    }
    fn job_done(&self, id: i32, file_num: i32) {
        self.queue_event(
            "job-done",
            &json!({"id":id,"file_num":file_num}).to_string(),
            &get_active_peer_id(),
        );
    }
    fn job_progress(&self, id: i32, file_num: i32, speed: f64, finished_size: f64) {
        self.queue_event(
            "job-progress",
            &json!({
                "id":id,
                "file_num":file_num,
                "speed":speed,
                "finished_size":finished_size
            })
            .to_string(),
            &get_active_peer_id(),
        );
    }
    fn clear_all_jobs(&self) {
        self.queue_event("clear-all-jobs", "{}", &get_active_peer_id());
    }
    fn new_message(&self, msg: String) {
        self.queue_event("chat-message", &msg, &get_active_peer_id());
    }
    fn update_transfer_list(&self) {
        self.queue_event("update-transfer-list", "{}", &get_active_peer_id());
    }
    fn load_last_job(&self, cnt: i32, job_json: &str, auto_start: bool) {
        self.queue_event(
            "load-last-job",
            &json!({"cnt":cnt,"job":job_json,"auto_start":auto_start}).to_string(),
            &get_active_peer_id(),
        );
    }
    fn update_folder_files(
        &self,
        id: i32,
        entries: &Vec<FileEntry>,
        path: String,
        is_local: bool,
        only_count: bool,
    ) {
        let detail = if only_count {
            let total_size: u64 = entries.iter().map(|entry| entry.size).sum();
            json!({
                "id":id,
                "path":path,
                "is_local":is_local,
                "only_count":only_count,
                "count":entries.len(),
                "total_size":total_size
            })
            .to_string()
        } else {
            crate::common::make_fd_to_json(id, path, entries)
        };
        self.queue_event("folder-files", &detail, &get_active_peer_id());
        self.queue_event("update-folder-files", &detail, &get_active_peer_id());
    }
    fn confirm_delete_files(&self, id: i32, i: i32, name: String) {
        self.queue_event(
            "confirm-delete-files",
            &json!({"id":id,"index":i,"name":name}).to_string(),
            &get_active_peer_id(),
        );
    }
    fn override_file_confirm(
        &self,
        id: i32,
        file_num: i32,
        to: String,
        is_upload: bool,
        is_identical: bool,
    ) {
        self.queue_event(
            "override-file-confirm",
            &json!({
                "id":id,
                "file_num":file_num,
                "to":to,
                "is_upload":is_upload,
                "is_identical":is_identical
            })
            .to_string(),
            &get_active_peer_id(),
        );
    }
    fn update_block_input_state(&self, on: bool) {
        if self.file_transfer {
            return;
        }
        self.queue_event(
            "block-input-state",
            if on { "on=true" } else { "on=false" },
            &get_active_peer_id(),
        );
    }

    fn adapt_size(&self) {}

    fn on_rgba(&self, display: usize, rgba: &mut scrap::ImageRgb) {
        if !self.is_current() || self.file_transfer {
            return;
        }
        publish_real_video_frame(display as c_int, rgba);
        self.queue_event(
            "video-frame",
            "Remote video frame received",
            &get_active_peer_id(),
        );
    }

    fn msgbox(&self, msgtype: &str, title: &str, text: &str, link: &str, retry: bool) {
        if !self.is_current() {
            return;
        }
        let peer_id = get_active_peer_id();
        if self.file_transfer {
            self.queue_event(
                "file-transfer-error",
                &json!({
                    "type": msgtype,
                    "title": title,
                    "text": text,
                    "link": link,
                    "retry": retry,
                })
                .to_string(),
                &peer_id,
            );
            return;
        }
        let detail =
            format!("type={msgtype}; title={title}; text={text}; link={link}; retry={retry}");
        if msgtype == "error" {
            update_connect_state("error", &peer_id, title, &detail, text);
            self.queue_event("session-error", &detail, &peer_id);
            if retry {
                self.queue_event("msgbox", &detail, &peer_id);
            }
        } else {
            self.queue_event("msgbox", &detail, &peer_id);
        }
    }

    fn cancel_msgbox(&self, tag: &str) {
        self.queue_event("cancel-msgbox", tag, &get_active_peer_id());
    }
    fn switch_back(&self, _id: &str) {}
    fn portable_service_running(&self, _running: bool) {}
    fn on_voice_call_started(&self) {
        self.queue_event("voice-call-started", "", &get_active_peer_id());
    }
    fn on_voice_call_closed(&self, reason: &str) {
        self.queue_event("voice-call-closed", reason, &get_active_peer_id());
    }
    fn on_voice_call_waiting(&self) {
        self.queue_event("voice-call-waiting", "", &get_active_peer_id());
    }
    fn on_voice_call_incoming(&self) {
        self.queue_event("voice-call-incoming", "", &get_active_peer_id());
    }
    fn get_rgba(&self, _display: usize) -> *const u8 {
        std::ptr::null()
    }
    fn next_rgba(&self, _display: usize) {}
    fn set_multiple_windows_session(&self, _sessions: Vec<WindowsSession>) {}
    fn set_current_display(&self, disp_idx: i32) {
        if self.file_transfer {
            return;
        }
        self.queue_event(
            "current-display",
            &format!("display={disp_idx}"),
            &get_active_peer_id(),
        );
    }
    fn update_record_status(&self, start: bool) {
        self.queue_event(
            "record-status",
            if start { "start=true" } else { "start=false" },
            &get_active_peer_id(),
        );
    }
    fn printer_request(&self, _id: i32, _path: String) {}
    fn handle_screenshot_resp(&self, sid: String, msg: String) {
        self.queue_event(
            "screenshot-response",
            &format!("sid={sid};msg={msg}"),
            &get_active_peer_id(),
        );
    }
    fn handle_terminal_response(&self, response: TerminalResponse) {
        use hbb_common::message_proto::terminal_response::Union;

        let peer_id = get_active_peer_id();
        match response.union {
            Some(Union::Opened(opened)) => {
                let detail = json!({
                    "type": "opened",
                    "terminal_id": opened.terminal_id,
                    "success": opened.success,
                    "message": opened.message,
                    "pid": opened.pid,
                    "service_id": opened.service_id,
                    "persistent_sessions": opened.persistent_sessions,
                    "replay_terminal_output": opened.replay_terminal_output,
                })
                .to_string();
                self.queue_event("terminal-response", &detail, &peer_id);
                self.queue_event(
                    if opened.success {
                        "terminal-opened"
                    } else {
                        "terminal-error"
                    },
                    &detail,
                    &peer_id,
                );
            }
            Some(Union::Data(data)) => {
                let output_data = if data.compressed {
                    hbb_common::compress::decompress(&data.data)
                } else {
                    data.data.to_vec()
                };
                let detail = json!({
                    "type": "data",
                    "terminal_id": data.terminal_id,
                    "dataBase64": crate::encode64(&output_data),
                    "compressed": false,
                })
                .to_string();
                self.queue_event("terminal-response", &detail, &peer_id);
                self.queue_event("terminal-output", &detail, &peer_id);
            }
            Some(Union::Closed(closed)) => {
                let detail = json!({
                    "type": "closed",
                    "terminal_id": closed.terminal_id,
                    "exit_code": closed.exit_code,
                })
                .to_string();
                self.queue_event("terminal-response", &detail, &peer_id);
                self.queue_event("terminal-closed", &detail, &peer_id);
            }
            Some(Union::Error(error)) => {
                let detail = json!({
                    "type": "error",
                    "terminal_id": error.terminal_id,
                    "message": error.message,
                })
                .to_string();
                self.queue_event("terminal-response", &detail, &peer_id);
                self.queue_event("terminal-error", &detail, &peer_id);
            }
            None => {}
            Some(_) => {
                self.queue_event("terminal-error", "{\"type\":\"unknown\"}", &peer_id);
            }
        }
    }
}

fn mark_peer_connected_with_cached_info(peer_id: &str) {
    let detail = peer_info_detail(peer_id);
    update_connect_state("connected", peer_id, "Connected", &detail, "");
    queue_event("session-connected", &detail, peer_id);
    queue_event("peer-info", &detail, peer_id);
}

/// Returns the boolean value of a session toggle option by key.
pub fn session_get_toggle_option(key: &str) -> bool {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        return session.get_toggle_option(key.to_owned());
    }
    option_is_enabled(&get_local_option(key))
}

/// Sets a local option value by key.
pub fn main_set_local_option(key: &str, value: &str) {
    local_options()
        .lock()
        .unwrap()
        .insert(key.to_owned(), value.to_owned());
    if is_harmony_persistent_option(key) {
        config::Config::set_option(key.to_owned(), value.to_owned());
        return;
    }
    if key == "access_token" || key == "user_info" || key == "lang" {
        LocalConfig::set_option(key.to_owned(), value.to_owned());
        return;
    }
    if key == "temporary-password" {
        *hbb_common::password_security::TEMPORARY_PASSWORD
            .write()
            .unwrap() = value.to_owned();
        return;
    }
    if key == "enable-lan-discovery"
        || key == "verification-method"
        || key == "approve-mode"
        || key.starts_with("custom-")
        || key == "rendezvous-servers"
    {
        config::Config::set_option(key.to_owned(), value.to_owned());
    }
}

/// Applies a session option with the given key and value.
/// Returns true if the option was applied successfully.
pub fn apply_session_option(key: &str, value: &str) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        set_local_option(key, value);
        queue_event(
            "session-option",
            &format!("queued-locally;key={key};value={value}"),
            "",
        );
        return false;
    };
    let applied = match key {
        "image-quality" => {
            let normalized = normalize_image_quality(value);
            if is_supported_image_quality(normalized) {
                session.save_image_quality(normalized.to_owned());
                true
            } else {
                false
            }
        }
        "custom-image-quality" => match value.parse::<i32>() {
            Ok(v) => {
                session.save_custom_image_quality(v);
                true
            }
            Err(_) => false,
        },
        "custom-fps" => match value.parse::<i32>() {
            Ok(v) => {
                session.set_custom_fps(v);
                true
            }
            Err(_) => false,
        },
        "keyboard-mode" => {
            if session.is_keyboard_mode_supported(value.to_owned()) {
                session.save_keyboard_mode(value.to_owned());
                true
            } else {
                false
            }
        }
        "codec-preference" => {
            let (vp8, av1, h264, h265) = session.alternative_codecs();
            let supported = match value {
                "auto" | "vp9" => true,
                "vp8" => vp8,
                "av1" => av1,
                "h264" => h264,
                "h265" => h265,
                _ => false,
            };
            if !supported {
                false
            } else {
                set_local_option(key, value);
                session.set_option(key.to_owned(), value.to_owned());
                session.update_supported_decodings();
                queue_event(
                    "codec-preference",
                    &format!(
                        "requested={value};stored={};vp8={vp8};av1={av1};h264={h264};h265={h265}",
                        session.get_option(key.to_owned())
                    ),
                    &get_active_peer_id(),
                );
                true
            }
        }
        "block-input" => {
            let command = if option_is_enabled(value) {
                "block-input"
            } else {
                "unblock-input"
            };
            session.toggle_option(command.to_owned());
            true
        }
        "privacy-mode" => {
            if session.is_privacy_mode_supported() {
                let enabled = option_is_enabled(value);
                if session.get_toggle_option(key.to_owned()) != enabled {
                    session.toggle_privacy_mode(String::new(), enabled);
                }
                true
            } else {
                false
            }
        }
        "record-session" => {
            session.record_screen(option_is_enabled(value));
            true
        }
        "take-screenshot" => {
            session.take_screenshot(0, format!("harmony-{}", current_timestamp_millis()));
            true
        }
        "switch-sides" => {
            session.switch_sides();
            true
        }
        "session-action" => false,
        _ if WIRE_TOGGLE_OPTIONS.contains(&key) => {
            set_session_toggle(&session, key, option_is_enabled(value));
            true
        }
        _ => {
            session.set_option(key.to_owned(), value.to_owned());
            true
        }
    };
    if applied {
        set_local_option(key, value);
        queue_event(
            "session-option",
            &format!("key={key};value={value}"),
            &get_active_peer_id(),
        );
    } else {
        queue_event(
            "session-option",
            &format!("failed=unsupported;key={key};value={value}"),
            &get_active_peer_id(),
        );
    }
    applied
}

fn option_is_enabled(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "y" || normalized == "yes" || normalized == "true" || normalized == "1"
}

fn set_session_toggle(session: &Session<HarmonyHandler>, key: &str, enabled: bool) {
    if session.get_toggle_option(key.to_owned()) != enabled {
        session.toggle_option(key.to_owned());
    }
}

fn normalize_image_quality(value: &str) -> &str {
    if value == "speed" {
        "low"
    } else {
        value
    }
}

fn is_supported_image_quality(value: &str) -> bool {
    matches!(value, "best" | "balanced" | "low" | "custom")
}

/// Marks the session as connected for the given peer.
pub fn mark_session_connected(peer_id: &str) {
    mark_peer_connected_with_cached_info(peer_id);
}

/// Reconnects the active official session.
pub fn session_reconnect(force_relay: bool) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    let peer_id = get_active_peer_id();
    if peer_id.trim().is_empty() {
        return false;
    }
    *latest_video_frame().lock().unwrap() = None;
    let detail = if force_relay {
        "Reconnecting through relay"
    } else {
        "Retrying connection"
    };
    update_connect_state("connecting", &peer_id, "Connecting", detail, "");
    queue_event(
        if force_relay {
            "reconnect-relay"
        } else {
            "reconnect"
        },
        detail,
        &peer_id,
    );
    session.reconnect(force_relay);
    true
}

/// Marks the session with an error message.
pub fn mark_session_error(message: &str) {
    let peer_id = get_active_peer_id();
    update_connect_state("error", &peer_id, message, message, message);
    queue_event("session-error", message, &peer_id);
}

/// Closes the current session.
pub fn session_close() {
    ACTIVE_SESSION_GENERATION.fetch_add(1, Ordering::SeqCst);
    close_file_transfer_session();
    active_session_password().lock().unwrap().clear();
    let peer_id = get_active_peer_id();
    if let Some(session) = active_session().lock().unwrap().as_ref() {
        session.send(Data::Close);
    }
    *active_session().lock().unwrap() = None;
    update_connect_state("idle", "", "Session closed", "Session closed by user", "");
    queue_event("session-closed", "Session closed by user", &peer_id);
}

/// Submits a session password for authentication.
pub fn session_login(password: &str, _remember: bool) -> bool {
    let peer_id = get_active_peer_id();
    if peer_id.is_empty() || password.is_empty() {
        return false;
    }
    update_connect_state(
        "login",
        &peer_id,
        "Password submitted; logging in",
        "Password submitted; logging in",
        "",
    );
    queue_event("login", "Password submitted; logging in", &peer_id);
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    *active_session_password().lock().unwrap() = password.to_owned();
    session.send(Data::Login((
        String::new(),
        String::new(),
        password.to_owned(),
        _remember,
    )));
    true
}

fn publish_real_video_frame(display: c_int, rgba: &scrap::ImageRgb) {
    if rgba.w == 0 || rgba.h == 0 || rgba.raw.is_empty() {
        return;
    }
    let mut guard = latest_video_frame().lock().unwrap();
    let next_frame_id = guard.as_ref().map(|frame| frame.frame_id + 1).unwrap_or(1);
    let format = match rgba.fmt {
        scrap::ImageFormat::ABGR => "abgr",
        scrap::ImageFormat::ARGB => "argb",
        scrap::ImageFormat::Raw => "bgra",
    }
    .to_owned();
    let stride = if rgba.align > 0 {
        rgba.align
    } else {
        rgba.w * 4
    };
    *guard = Some(VideoFrameState {
        frame_id: next_frame_id,
        display,
        width: rgba.w,
        height: rgba.h,
        stride,
        bytes: rgba.raw.clone(),
        timestamp: current_timestamp_millis(),
        format,
    });
}

/// Restarts the remote device.
pub fn session_restart_remote_device() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=restart",
            "",
        );
        return false;
    };
    session.restart_remote_device();
    queue_event("session-command", "command=restart", &get_active_peer_id());
    true
}

/// Locks the remote screen.
pub fn session_lock_screen() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=lock-screen",
            "",
        );
        return false;
    };
    session.lock_screen();
    queue_event(
        "session-command",
        "command=lock-screen",
        &get_active_peer_id(),
    );
    true
}

/// Opens a terminal with the given ID and dimensions.
/// Returns true if the terminal was opened successfully.
pub fn session_open_terminal(terminal_id: c_int, rows: c_int, cols: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "terminal-error",
            &json!({"type":"error","terminal_id":terminal_id,"message":"no active session"})
                .to_string(),
            "",
        );
        return false;
    };
    let normalized_rows = if rows > 0 { rows as u32 } else { 24 };
    let normalized_cols = if cols > 0 { cols as u32 } else { 80 };
    session.open_terminal(terminal_id, normalized_rows, normalized_cols);
    queue_event(
        "terminal-response",
        &json!({
            "type":"open-requested",
            "terminal_id":terminal_id,
            "rows":normalized_rows,
            "cols":normalized_cols
        })
        .to_string(),
        &get_active_peer_id(),
    );
    true
}

/// Sends input data to the terminal with the given ID.
/// Returns true if the input was sent successfully.
pub fn session_send_terminal_input(terminal_id: c_int, data: &str) -> bool {
    if data.is_empty() {
        return false;
    }
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "terminal-error",
            &json!({"type":"error","terminal_id":terminal_id,"message":"no active session"})
                .to_string(),
            "",
        );
        return false;
    };
    session.send_terminal_input(terminal_id, data.to_owned());
    true
}

/// Resizes the terminal with the given ID to the specified dimensions.
/// Returns true if the resize was successful.
pub fn session_resize_terminal(terminal_id: c_int, rows: c_int, cols: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "terminal-error",
            &json!({"type":"error","terminal_id":terminal_id,"message":"no active session"})
                .to_string(),
            "",
        );
        return false;
    };
    let normalized_rows = if rows > 0 { rows as u32 } else { 24 };
    let normalized_cols = if cols > 0 { cols as u32 } else { 80 };
    session.resize_terminal(terminal_id, normalized_rows, normalized_cols);
    true
}

/// Closes the terminal with the given ID.
/// Returns true if the terminal was closed successfully.
pub fn session_close_terminal(terminal_id: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "terminal-error",
            &json!({"type":"error","terminal_id":terminal_id,"message":"no active session"})
                .to_string(),
            "",
        );
        return false;
    };
    session.close_terminal(terminal_id);
    true
}

/// Reads the remote directory at the given path.
/// Returns true if the read was initiated successfully.
pub fn session_read_remote_dir(path: &str, include_hidden: bool) -> bool {
    let normalized = path.trim();
    if file_transfer_session().lock().unwrap().is_none() {
        return start_file_transfer_session(normalized, include_hidden);
    }
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    session.read_remote_dir(normalized.to_owned(), include_hidden);
    queue_event(
        "file-transfer",
        &json!({"action":"read-dir","path":normalized,"includeHidden":include_hidden}).to_string(),
        &get_active_peer_id(),
    );
    true
}

/// Creates a remote directory at the given path.
/// Returns true if the directory was created successfully.
pub fn session_create_dir(path: &str) -> bool {
    let normalized = path.trim();
    if normalized.is_empty() {
        queue_event(
            "file-transfer",
            "failed=empty-path; action=create-dir",
            &get_active_peer_id(),
        );
        return false;
    }
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "file-transfer",
            "failed=no-active-session; action=create-dir",
            "",
        );
        return false;
    };
    session.create_dir(next_bridge_job_id(), normalized.to_owned(), true);
    queue_event(
        "create-remote-dir",
        &json!({"action":"create-dir","path":normalized}).to_string(),
        &get_active_peer_id(),
    );
    true
}

/// Deletes a remote path (file or directory).
/// Returns true if the deletion was successful.
pub fn delete_remote_path(path: &str, is_directory: bool) -> bool {
    let normalized = path.trim();
    if normalized.is_empty() {
        queue_event(
            "file-transfer",
            "failed=empty-path; action=delete",
            &get_active_peer_id(),
        );
        return false;
    }
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "file-transfer",
            "failed=no-active-session; action=delete",
            "",
        );
        return false;
    };
    let job_id = next_bridge_job_id();
    if is_directory {
        session.remove_dir_all(job_id, normalized.to_owned(), true, false);
    } else {
        session.remove_file(job_id, normalized.to_owned(), 0, true);
    }
    queue_event(
        "delete-remote-path",
        &json!({"action":"delete","path":normalized,"isDirectory":is_directory}).to_string(),
        &get_active_peer_id(),
    );
    true
}

/// Starts a file transfer task.
/// Returns true if the task was started successfully.
pub fn session_send_files(path: &str, to: &str, is_remote: bool) -> bool {
    let normalized_path = path.trim();
    let normalized_to = to.trim();
    if normalized_path.is_empty() || normalized_to.is_empty() {
        queue_event(
            "file-transfer",
            "failed=empty-path; action=start",
            &get_active_peer_id(),
        );
        return false;
    }
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "file-transfer",
            "failed=no-active-session; action=start",
            "",
        );
        return false;
    };
    let job_id = next_bridge_job_id();
    session.send_files(
        job_id,
        hbb_common::fs::JobType::Generic.into(),
        normalized_path.to_owned(),
        normalized_to.to_owned(),
        1,
        false,
        is_remote,
    );
    queue_event(
        "file-transfer-start",
        &json!({
            "id":job_id,
            "action":"start",
            "path":normalized_path,
            "to":normalized_to,
            "is_remote":is_remote
        })
        .to_string(),
        &get_active_peer_id(),
    );
    true
}

/// Queries online status for the given peer ID JSON payload.
/// Returns true if the query was submitted successfully.
pub fn query_onlines(ids_json: &str) -> bool {
    let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_json) else {
        queue_event(
            "query-onlines-result",
            "{\"onlines\":[],\"offlines\":[]}",
            "",
        );
        return false;
    };
    std::thread::spawn(move || {
        let ids_for_error = ids.clone();
        let Ok(rt) = hbb_common::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            queue_event(
                "query-onlines-result",
                &json!({"onlines":[],"offlines":ids_for_error}).to_string(),
                "",
            );
            return;
        };
        rt.block_on(client::peer_online::query_online_states(
            ids,
            |onlines: Vec<String>, offlines: Vec<String>| {
                queue_event(
                    "query-onlines-result",
                    &json!({"onlines":onlines,"offlines":offlines}).to_string(),
                    "",
                );
            },
        ));
    });
    true
}

/// Starts LAN peer discovery.
pub fn main_discover() {
    config::Config::set_option("enable-lan-discovery".to_owned(), "Y".to_owned());
    crate::RendezvousMediator::restart();
    std::thread::spawn(|| {
        if let Err(err) = crate::lan::discover() {
            queue_event(
                "lan-discovery-error",
                &format!("LAN discovery failed: {err}"),
                "",
            );
        } else {
            queue_event("lan-discovery-done", "LAN discovery completed", "");
        }
    });
}

/// Loads discovered LAN peers as JSON.
pub fn main_load_lan_peers() -> String {
    let loaded = LanPeers::load();
    let peers = loaded.peers;
    let diag = format!("count={}", peers.len());
    queue_event("lan-load-diag", &diag, "");
    json!(peers
        .into_iter()
        .map(|peer| json!({
            "id": peer.id,
            "username": peer.username,
            "hostname": peer.hostname,
            "platform": peer.platform,
            "online": peer.online,
            "ipMac": peer.ip_mac,
        }))
        .collect::<Vec<_>>())
    .to_string()
}

/// Removes a discovered peer from the LAN list.
/// Returns true if a peer was removed.
pub fn main_remove_discovered(peer_id: &str) -> bool {
    let mut peers = LanPeers::load().peers;
    let before = peers.len();
    peers.retain(|peer| peer.id != peer_id);
    if peers.len() == before {
        return false;
    }
    LanPeers::store(&peers);
    true
}

// ============================================================
// Extended bridge functions (aligned with official wire_ API)
// All are thin wrappers calling existing upstream Session/ui_interface
// methods — zero additional compile size since upstream is already
// linked into the staticlib.
// ============================================================

pub fn session_send2fa(code: &str, trust_this_device: bool) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=send2fa",
            "",
        );
        return false;
    };
    session.send2fa(code.to_owned(), trust_this_device);
    queue_event("session-command", "command=send2fa", &get_active_peer_id());
    true
}

pub fn session_toggle_option(name: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.toggle_option(name.to_owned());
    queue_event(
        "session-option",
        &format!("toggle;key={name}"),
        &get_active_peer_id(),
    );
}

pub fn session_toggle_privacy_mode(impl_key: &str, on: bool) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=toggle-privacy-mode",
            "",
        );
        return false;
    };
    session.toggle_privacy_mode(impl_key.to_owned(), on);
    queue_event(
        "session-command",
        &format!("command=toggle-privacy-mode;on={on}"),
        &get_active_peer_id(),
    );
    true
}

pub fn session_switch_display(display: c_int) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=switch-display",
            "",
        );
        return false;
    };
    session.switch_display(display);
    queue_event(
        "session-command",
        &format!("command=switch-display;display={display}"),
        &get_active_peer_id(),
    );
    true
}

pub fn session_enter_or_leave() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=enter-or-leave",
            "",
        );
        return false;
    };
    let keyboard_mode = session.get_keyboard_mode();
    session.enter(keyboard_mode);
    queue_event(
        "session-command",
        "command=enter-or-leave",
        &get_active_peer_id(),
    );
    true
}

pub fn session_leave() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=leave",
            "",
        );
        return false;
    };
    let keyboard_mode = session.get_keyboard_mode();
    session.leave(keyboard_mode);
    queue_event("session-command", "command=leave", &get_active_peer_id());
    true
}

pub fn session_set_size(_display: usize, _width: usize, _height: usize) {
    // set_size is VideoRenderer method, not Session; stub for OHOS
}

pub fn session_change_resolution(display: c_int, width: c_int, height: c_int) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.change_resolution(display, width, height);
}

pub fn session_elevate_direct() {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.elevate_direct();
}

pub fn session_elevate_with_logon(username: &str, password: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.elevate_with_logon(username.to_owned(), password.to_owned());
}

pub fn session_switch_sides() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=switch-sides",
            "",
        );
        return false;
    };
    session.switch_sides();
    queue_event(
        "session-command",
        "command=switch-sides",
        &get_active_peer_id(),
    );
    true
}

pub fn session_take_screenshot(display: usize) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    session.take_screenshot(
        display as i32,
        format!("harmony-{}", current_timestamp_millis()),
    );
    queue_event(
        "session-command",
        "command=take-screenshot",
        &get_active_peer_id(),
    );
    true
}

pub fn session_record_screen(start: bool) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=record-screen",
            "",
        );
        return false;
    };
    session.record_screen(start);
    queue_event(
        "session-command",
        &format!("command=record-screen;start={start}"),
        &get_active_peer_id(),
    );
    true
}

pub fn session_get_is_recording() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    session.is_recording()
}

pub fn session_request_voice_call() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=request-voice-call",
            "",
        );
        return false;
    };
    session.request_voice_call();
    queue_event(
        "session-command",
        "command=request-voice-call",
        &get_active_peer_id(),
    );
    true
}

pub fn session_close_voice_call() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        queue_event(
            "session-command",
            "failed=no-active-session; command=close-voice-call",
            "",
        );
        return false;
    };
    session.close_voice_call();
    queue_event(
        "session-command",
        "command=close-voice-call",
        &get_active_peer_id(),
    );
    true
}

pub fn session_add_port_forward(local_port: c_int, remote_host: &str, remote_port: c_int) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.add_port_forward(local_port, remote_host.to_owned(), remote_port);
}

pub fn session_remove_port_forward(local_port: c_int) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.remove_port_forward(local_port);
}

pub fn session_new_rdp() {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.new_rdp();
}

pub fn session_remove_file(act_id: i32, path: &str, file_num: i32, is_remote: bool) {
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.remove_file(act_id, path.to_owned(), file_num, is_remote);
}

pub fn session_rename_file(act_id: i32, path: &str, new_name: &str, is_remote: bool) {
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.rename_file(act_id, path.to_owned(), new_name.to_owned(), is_remote);
}

pub fn session_cancel_job(act_id: i32) {
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.cancel_job(act_id);
}

pub fn session_resume_job(act_id: i32, is_remote: bool) {
    let Some(session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.resume_job(act_id, is_remote);
}

pub fn session_set_confirm_override_file(
    act_id: i32,
    file_num: i32,
    need_override: bool,
    remember: bool,
    is_upload: bool,
) {
    let Some(mut session) = file_transfer_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.set_write_override(act_id, file_num, need_override, remember, is_upload);
}

pub fn session_send_note(note: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.send_note(note.to_owned());
}

pub fn session_input_string(value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.input_string(value);
}

pub fn session_input_os_password(pass: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.input_os_password(pass.to_owned(), true);
}

pub fn session_load_last_transfer_jobs() {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.load_last_jobs();
}

pub fn session_get_view_style() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_view_style()
}

pub fn session_set_view_style(value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_view_style(value.to_owned());
}

pub fn session_get_scroll_style() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_scroll_style()
}

pub fn session_set_scroll_style(value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_scroll_style(value.to_owned());
}

pub fn session_get_image_quality() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_image_quality()
}

pub fn session_set_image_quality(value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_image_quality(value.to_owned());
}

pub fn session_get_keyboard_mode() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_keyboard_mode()
}

pub fn session_set_keyboard_mode(value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_keyboard_mode(value.to_owned());
}

pub fn session_get_custom_image_quality() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    let v = session.get_custom_image_quality();
    format!("{:?}", v)
}

pub fn session_set_custom_image_quality(value: i32) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_custom_image_quality(value);
}

pub fn session_set_custom_fps(fps: i32) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.set_custom_fps(fps);
}

pub fn session_get_trackpad_speed() -> i32 {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return 0;
    };
    session.get_trackpad_speed()
}

pub fn session_set_trackpad_speed(value: i32) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_trackpad_speed(value);
}

pub fn session_get_flutter_option(k: &str) -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_flutter_option(k.to_owned())
}

pub fn session_set_flutter_option(k: &str, v: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_flutter_option(k.to_owned(), v.to_owned());
}

pub fn session_get_reverse_mouse_wheel_sync() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_reverse_mouse_wheel()
}

pub fn session_set_reverse_mouse_wheel(value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.save_reverse_mouse_wheel(value.to_owned());
}

pub fn session_get_option(k: &str) -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_option(k.to_owned())
}

pub fn session_set_option(k: &str, v: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.set_option(k.to_owned(), v.to_owned());
}

pub fn session_get_peer_option(name: &str) -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_option(name.to_owned())
}

pub fn session_peer_option(name: &str, value: &str) {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.set_option(name.to_owned(), value.to_owned());
}

pub fn session_is_keyboard_mode_supported(mode: &str) -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    session.is_keyboard_mode_supported(mode.to_owned())
}

pub fn session_get_platform(is_remote: bool) -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    session.get_platform(is_remote)
}

pub fn session_get_remember() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    session.get_remember()
}

pub fn session_get_enable_trusted_devices() -> bool {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return false;
    };
    session.get_enable_trusted_devices()
}

pub fn session_get_alternative_codecs() -> String {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return String::new();
    };
    let (vp8, av1, h264, h265) = session.alternative_codecs();
    json!({"vp8":vp8,"av1":av1,"h264":h264,"h265":h265}).to_string()
}

pub fn session_change_prefer_codec() {
    let Some(session) = active_session().lock().unwrap().as_ref().cloned() else {
        return;
    };
    session.update_supported_decodings();
}

// ---- main_ global functions (thin wrappers over ui_interface) ----

pub fn main_get_option(key: &str) -> String {
    config::Config::get_option(key)
}

pub fn main_set_option(key: &str, value: &str) {
    config::Config::set_option(key.to_owned(), value.to_owned());
}

pub fn main_get_options() -> String {
    let map = config::Config::get_options();
    json!(map).to_string()
}

pub fn main_get_my_id() -> String {
    config::Config::get_id()
}

pub fn main_get_uuid() -> String {
    let uuid = hbb_common::get_uuid();
    uuid.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn main_get_version() -> String {
    crate::VERSION.to_owned()
}

pub fn main_get_fingerprint() -> String {
    crate::common::pk_to_fingerprint(config::Config::get_key_pair().1)
}

pub fn main_get_api_server() -> String {
    let api = config::Config::get_option("api-server");
    crate::get_api_server(api, String::new())
}

pub fn main_get_temporary_password() -> String {
    hbb_common::password_security::temporary_password()
}

pub fn main_set_permanent_password_with_result(password: &str) -> bool {
    config::Config::set_permanent_password(password)
}

pub fn main_update_temporary_password() {
    hbb_common::password_security::update_temporary_password();
}

pub fn main_test_if_valid_server(server: &str) -> String {
    hbb_common::socket_client::test_if_valid_server(server, false)
}

pub fn main_get_connect_status() -> String {
    json!({
        "statusNum": 0,
        "errMsg": "",
        "linkNum": 0,
    })
    .to_string()
}

pub fn main_is_using_public_server() -> bool {
    crate::common::using_public_server()
}

pub fn main_forget_password(id: &str) {
    let mut c = PeerConfig::load(id);
    c.password.clear();
    c.store(id);
}

pub fn main_peer_has_password(id: &str) -> bool {
    !PeerConfig::load(id).password.is_empty()
}

pub fn main_peer_exists(id: &str) -> bool {
    hbb_common::config::PeerConfig::exists(id)
}

pub fn main_set_peer_alias(id: &str, alias: &str) {
    let mut c = PeerConfig::load(id);
    c.options.insert("alias".to_owned(), alias.to_owned());
    c.store(id);
}

pub fn main_set_peer_option(id: &str, key: &str, value: &str) {
    let mut c = PeerConfig::load(id);
    c.options.insert(key.to_owned(), value.to_owned());
    c.store(id);
}

pub fn main_remove_peer(id: &str) {
    hbb_common::config::PeerConfig::remove(id);
}

pub fn main_get_new_stored_peers() -> String {
    let set = config::NEW_STORED_PEER_CONFIG.lock().unwrap();
    let v: Vec<_> = set.iter().collect();
    json!(v).to_string()
}

pub fn main_load_recent_peers() {
    // TODO: push recent peers event to UI
}

pub fn main_get_langs() -> String {
    // crate::lang::LANGS not available on OHOS; return empty array
    "[]".to_owned()
}

pub fn main_get_error() -> String {
    String::new()
}

pub fn main_get_build_date() -> String {
    crate::BUILD_DATE.to_owned()
}

pub fn main_get_license() -> String {
    String::new()
}

pub fn main_get_app_name() -> String {
    "RustDesk".to_owned()
}

pub fn main_has_hwcodec() -> bool {
    cfg!(feature = "hwcodec") || cfg!(feature = "mediacodec")
}

pub fn main_generate2fa() -> String {
    crate::auth_2fa::generate2fa()
}

pub fn main_verify2fa(code: &str) -> bool {
    crate::auth_2fa::verify2fa(code.to_owned())
}

pub fn main_get_trusted_devices() -> String {
    config::Config::get_option("trusted-devices")
}

pub fn main_clear_trusted_devices() {
    config::Config::set_option("trusted-devices".to_owned(), "".to_owned());
}

pub fn main_set_user_default_option(key: &str, value: &str) {
    config::Config::set_option(key.to_owned(), value.to_owned());
}

pub fn main_get_user_default_option(key: &str) -> String {
    config::Config::get_option(key)
}

pub fn main_resolve_avatar_url(avatar: &str) -> String {
    avatar.to_owned()
}

pub fn main_get_login_device_info() -> String {
    String::new()
}

pub fn main_get_hard_option(key: &str) -> String {
    // HARD_SETTINGS is in hbb_common::config; use get_option as fallback
    config::Config::get_option(key)
}

pub fn main_get_buildin_option(key: &str) -> String {
    crate::common::get_builtin_option(key)
}

pub fn main_get_common(key: &str) -> String {
    config::Config::get_option(key)
}

pub fn main_set_common(key: &str, value: &str) {
    config::Config::set_option(key.to_owned(), value.to_owned());
}

pub fn main_check_connect_status() {
    // TODO: implement connect status check for OHOS
}

pub fn main_stop_service() {
    // TODO: RendezvousMediator::restart may need special conditions on OHOS
    config::Config::set_option("stop-service".to_owned(), "Y".to_owned());
}

pub fn main_on_main_window_close() {}

pub fn main_wol(id: &str) {
    crate::lan::send_wol(id.to_owned());
}

pub fn main_http_request(_url: &str, _method: &str, _body: &str, _header: &str) {
    // TODO: implement via crate::http_request
}

// ============================================================
// Session type-check methods (exposed from Session)
// ============================================================

pub fn session_is_file_transfer() -> bool {
    file_transfer_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| s.is_file_transfer())
}

pub fn session_is_terminal() -> bool {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| s.is_terminal())
}

pub fn session_is_port_forward() -> bool {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| s.is_port_forward())
}

pub fn session_is_rdp() -> bool {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| s.is_rdp())
}

pub fn session_is_view_camera() -> bool {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| s.is_view_camera())
}

pub fn session_toggle_virtual_display(index: i32, on: bool) {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.toggle_virtual_display(index, on);
    }
}

pub fn session_get_audit_server(typ: &str) -> String {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(String::new(), |s| s.get_audit_server(typ.to_owned()))
}

pub fn session_send_selected_session_id(sid: &str) {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.send_selected_session_id(sid.to_owned());
    }
}

pub fn session_get_conn_token() -> String {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(String::new(), |s| s.get_conn_token().unwrap_or_default())
}

pub fn session_handle_flutter_key_event(
    keyboard_mode: &str,
    character: &str,
    usb_hid: i32,
    lock_modes: i32,
    down_or_up: bool,
) {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.handle_flutter_key_event(keyboard_mode, character, usb_hid, lock_modes, down_or_up);
    }
}

pub fn session_handle_flutter_raw_key_event(
    keyboard_mode: &str,
    name: &str,
    platform_code: i32,
    position_code: i32,
    lock_modes: i32,
    down_or_up: bool,
) {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.handle_flutter_raw_key_event(
            keyboard_mode,
            name,
            platform_code,
            position_code,
            lock_modes,
            down_or_up,
        );
    }
}

pub fn session_send_touch_scale(scale: i32, alt: bool, ctrl: bool, shift: bool, command: bool) {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.send_touch_scale(scale, alt, ctrl, shift, command);
    }
}

pub fn session_send_touch_pan_event(
    event: &str,
    x: i32,
    y: i32,
    alt: bool,
    ctrl: bool,
    shift: bool,
    command: bool,
) {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.send_touch_pan_event(event, x, y, alt, ctrl, shift, command);
    }
}

pub fn session_refresh() {
    if let Some(session) = active_session().lock().unwrap().as_ref().cloned() {
        session.refresh_video(0);
    }
}

pub fn session_get_peer_version() -> String {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(String::new(), |s| s.get_peer_version().to_string())
}

pub fn session_get_path_sep() -> String {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(String::new(), |s| s.get_path_sep(true).to_string())
}

pub fn session_is_restarting_remote_device() -> bool {
    active_session()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| s.is_restarting_remote_device())
}

// ============================================================
// cm_* functions (connection manager - controlled side)
// All stubs: cm module requires flutter feature
// ============================================================

/// Initializes the connection manager. Stub: cm module requires flutter feature.
pub fn cm_init() {
    // cm module requires flutter feature
}

/// Returns the clients state as JSON. Stub: cm module requires flutter feature.
pub fn cm_get_clients_state() -> String {
    // cm module requires flutter feature
    String::new()
}

/// Checks clients length. Stub: cm module requires flutter feature.
pub fn cm_check_clients_length(_length: usize) -> String {
    // cm module requires flutter feature
    String::new()
}

/// Returns the number of connected clients. Stub: cm module requires flutter feature.
pub fn cm_get_clients_length() -> usize {
    // cm module requires flutter feature
    0
}

/// Sends a chat message from the controlled side. Stub: cm module requires flutter feature.
pub fn cm_send_chat(_conn_id: i32, _msg: &str) {
    // cm module requires flutter feature
}

/// Responds to a login request. Stub: cm module requires flutter feature.
pub fn cm_login_res(_conn_id: i32, _res: bool) {
    // cm module requires flutter feature
}

/// Closes a connection. Stub: cm module requires flutter feature.
pub fn cm_close_connection(_conn_id: i32) {
    // cm module requires flutter feature
}

/// Removes a disconnected connection. Stub: cm module requires flutter feature.
pub fn cm_remove_disconnected_connection(_conn_id: i32) {
    // cm module requires flutter feature
}

/// Checks click time for auto-disconnect. Stub: cm module requires flutter feature.
pub fn cm_check_click_time(_conn_id: i32) {
    // cm module requires flutter feature
}

/// Returns the click time threshold. Stub: cm module requires flutter feature.
pub fn cm_get_click_time() -> f64 {
    // cm module requires flutter feature
    0.0
}

/// Switches a permission for a connection. Stub: cm module requires flutter feature.
pub fn cm_switch_permission(_conn_id: i32, _name: &str, _enabled: bool) {
    // cm module requires flutter feature
}

/// Returns whether elevation is possible. Stub: cm module requires flutter feature.
pub fn cm_can_elevate() -> bool {
    // cm module requires flutter feature
    false
}

/// Elevates portable service. Stub: cm module requires flutter feature.
pub fn cm_elevate_portable(_conn_id: i32) {
    // cm module requires flutter feature
}

/// Switches back from elevated session. Stub: cm module requires flutter feature.
pub fn cm_switch_back(_conn_id: i32) {
    // cm module requires flutter feature
}

/// Gets a cm config value. Stub: cm module requires flutter feature.
pub fn cm_get_config(_name: &str) -> String {
    // cm module requires flutter feature
    String::new()
}

/// Handles an incoming voice call. Stub: cm module requires flutter feature.
pub fn cm_handle_incoming_voice_call(_id: i32, _accept: bool) {
    // cm module requires flutter feature
}

/// Closes a voice call. Stub: cm module requires flutter feature.
pub fn cm_close_voice_call(_id: i32) {
    // cm module requires flutter feature
}

// ============================================================
// plugin_* functions (all stubs: plugin system requires flutter)
// ============================================================

pub fn plugin_event(_id: &str, _peer: &str, _msg: &str) {
    // plugin system requires flutter feature
}

pub fn plugin_register_event_stream(_id: &str, _peer: &str) {
    // plugin system requires flutter feature
}

pub fn plugin_get_session_option(_id: &str, _key: &str) -> String {
    // plugin system requires flutter feature
    String::new()
}

pub fn plugin_set_session_option(_id: &str, _key: &str, _value: &str) {
    // plugin system requires flutter feature
}

pub fn plugin_get_shared_option(_id: &str, _key: &str) -> String {
    // plugin system requires flutter feature
    String::new()
}

pub fn plugin_set_shared_option(_id: &str, _key: &str, _value: &str) {
    // plugin system requires flutter feature
}

pub fn plugin_reload(_id: &str) {
    // plugin system requires flutter feature
}

pub fn plugin_enable(_id: &str, _enable: bool) {
    // plugin system requires flutter feature
}

pub fn plugin_is_enabled(_id: &str) -> bool {
    // plugin system requires flutter feature
    false
}

pub fn plugin_feature_is_enabled(_id: &str) -> bool {
    // plugin system requires flutter feature
    false
}

pub fn plugin_sync_ui(_id: &str) {
    // plugin system requires flutter feature
}

pub fn plugin_list_reload() {
    // plugin system requires flutter feature
}

pub fn plugin_install(_id: &str, _b: bool) {
    // plugin system requires flutter feature
}

// ============================================================
// install_* functions (not needed on OHOS)
// ============================================================

/// Installs the application. Stub: not needed on OHOS.
pub fn install_install_me(_path: &str, _options: &str, _exe: &str) {
    // not needed on OHOS
}

/// Returns install options. Stub: not needed on OHOS.
pub fn install_install_options() -> String {
    // not needed on OHOS
    String::new()
}

/// Returns the install path. Stub: not needed on OHOS.
pub fn install_install_path() -> String {
    // not needed on OHOS
    String::new()
}

/// Returns whether to run without install. Stub: not needed on OHOS.
pub fn install_run_without_install() -> bool {
    // not needed on OHOS
    false
}

/// Returns whether to show the run-without-install option. Stub: not needed on OHOS.
pub fn install_show_run_without_install() -> bool {
    // not needed on OHOS
    false
}

// ============================================================
// is_* functions (branding/feature flags, all false for OHOS)
// ============================================================

/// Returns Whether this is a custom client build.
pub fn is_custom_client() -> bool {
    false
}

/// Returns Whether AB (address book) is disabled.
pub fn is_disable_ab() -> bool {
    false
}

/// Returns Whether account login is disabled.
pub fn is_disable_account() -> bool {
    false
}

/// Returns Whether the group panel is disabled.
pub fn is_disable_group_panel() -> bool {
    false
}

/// Returns Whether installation is disabled.
pub fn is_disable_installation() -> bool {
    false
}

/// Returns Whether settings are disabled.
pub fn is_disable_settings() -> bool {
    false
}

/// Returns Whether this is an incoming-only build.
pub fn is_incoming_only() -> bool {
    false
}

/// Returns Whether this is an outgoing-only build.
pub fn is_outgoing_only() -> bool {
    false
}

/// Returns Whether a preset password is configured.
pub fn is_preset_password() -> bool {
    false
}

/// Returns Whether a preset password for mobile only is configured.
pub fn is_preset_password_mobile_only() -> bool {
    false
}

/// Returns Whether SELinux is enforcing. Not applicable on OHOS.
pub fn is_selinux_enforcing() -> bool {
    false
}

/// Returns Whether multi UI sessions are supported.
pub fn is_support_multi_ui_session() -> bool {
    false
}

// ============================================================
// main_* missing functions
// ============================================================

pub fn main_change_id(_id: &str) {
    // TODO: implement via config
}

pub fn main_change_language(lang: &str) {
    config::Config::set_option("language".to_owned(), lang.to_owned());
}

pub fn main_change_theme(dark: &str) {
    config::Config::set_option("theme".to_owned(), dark.to_owned());
}

pub fn main_get_displays() -> String {
    // TODO: implement display enumeration for OHOS
    String::new()
}

pub fn main_get_printer_names() -> String {
    String::new()
}

pub fn main_get_socks() -> String {
    "[]".to_owned()
}

pub fn main_set_socks(_proxy: &str, _username: &str, _password: &str) {
    // TODO: implement proxy configuration
}

pub fn main_get_proxy_status() -> bool {
    false
}

pub fn main_get_app_name_sync() -> String {
    "RustDesk".to_owned()
}

pub fn main_get_new_version() -> String {
    String::new()
}

pub fn main_get_home_dir() -> String {
    config::Config::get_home().to_string_lossy().to_string()
}

pub fn main_init(app_dir: String, custom_client_config: String) {
    initialize_runtime(&app_dir, &custom_client_config);
}

pub fn main_device_id() -> String {
    config::Config::get_id()
}

pub fn main_device_name() -> String {
    String::new()
}

pub fn main_is_installed() -> bool {
    false
}

pub fn main_is_installed_daemon() -> bool {
    false
}

pub fn main_is_root() -> bool {
    false
}

pub fn main_is_process_trusted() -> bool {
    true
}

pub fn main_is_can_screen_recording() -> bool {
    true
}

pub fn main_is_can_input_monitoring() -> bool {
    true
}

pub fn main_current_is_wayland() -> bool {
    false
}

pub fn main_is_login_wayland() -> bool {
    false
}

pub fn main_has_vram() -> bool {
    false
}

pub fn main_supported_hwdecodings() -> String {
    "[false,false]".to_owned()
}

pub fn main_check_hwcodec() {
    // TODO: implement hwcodec check for OHOS
}

pub fn main_create_shortcut() -> bool {
    false
}

pub fn main_get_mouse_time() -> i64 {
    0
}

pub fn main_check_mouse_time() -> bool {
    false
}

pub fn main_get_async_status() -> String {
    String::new()
}

pub fn main_get_lan_peers() -> String {
    json!(LanPeers::load().peers).to_string()
}

pub fn main_get_last_remote_id() -> String {
    LocalConfig::get_option("last-remote-id")
}

pub fn main_get_fav() -> String {
    String::new()
}

pub fn main_store_fav(fav: &str) {
    config::Config::set_option("fav".to_owned(), fav.to_owned());
}

pub fn main_get_peer_sync(id: &str) -> String {
    json!(PeerConfig::load(id)).to_string()
}

pub fn main_get_peer_flutter_option_sync(id: &str, k: &str) -> String {
    PeerConfig::load(id)
        .options
        .get(k)
        .cloned()
        .unwrap_or_default()
}

pub fn main_set_peer_flutter_option_sync(id: &str, k: &str, v: &str) {
    let mut c = PeerConfig::load(id);
    c.options.insert(k.to_owned(), v.to_owned());
    c.store(id);
}

pub fn main_get_peer_option_sync(id: &str, k: &str) -> String {
    PeerConfig::load(id)
        .options
        .get(k)
        .cloned()
        .unwrap_or_default()
}

pub fn main_set_peer_option_sync(id: &str, k: &str, v: &str) {
    let mut c = PeerConfig::load(id);
    c.options.insert(k.to_owned(), v.to_owned());
    c.store(id);
}

pub fn main_remove_trusted_devices(_json: &str) {
    // TODO: implement trusted devices removal
}

pub fn main_has_valid_2fa_sync() -> bool {
    crate::auth_2fa::get_2fa(None).is_some()
}

pub fn main_has_valid_bot_sync() -> bool {
    false
}

pub fn main_verify_bot(_token: &str) -> bool {
    false
}

pub fn main_max_encrypt_len() -> usize {
    128
}

pub fn main_get_unlock_pin() -> String {
    String::new()
}

pub fn main_set_unlock_pin(_pin: &str) {
    // TODO: implement unlock PIN
}

pub fn main_option_synced() -> bool {
    false
}

pub fn main_support_remove_wallpaper() -> bool {
    false
}

pub fn main_test_wallpaper() -> bool {
    false
}

pub fn main_supported_privacy_mode_impls() -> String {
    "[]".to_owned()
}

pub fn main_default_privacy_mode_impl() -> String {
    String::new()
}

pub fn main_is_option_fixed(_key: &str) -> bool {
    false
}

pub fn main_get_use_texture_render() -> bool {
    false
}

pub fn main_has_file_clipboard() -> bool {
    false
}

pub fn main_has_gpu_texture_render() -> bool {
    false
}

pub fn main_audio_support_loopback() -> bool {
    false
}

pub fn main_is_share_rdp() -> bool {
    false
}

pub fn main_set_share_rdp(_v: bool) {
    // not applicable on OHOS
}

pub fn main_is_installed_lower_version() -> bool {
    false
}

pub fn main_get_software_update_url() -> String {
    String::new()
}

pub fn main_handle_relay_id(_id: &str) {
    // TODO: implement relay ID handling
}

pub fn main_hide_dock() {
    // not applicable on OHOS
}

pub fn main_set_cursor_position(x: i32, y: i32) {
    let _ = crate::platform::set_cursor_pos(x, y);
}

pub fn main_clip_cursor() {
    // TODO: implement cursor clip
}

pub fn main_get_env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

pub fn main_set_env(key: &str, value: &str) {
    std::env::set_var(key, value);
}

pub fn main_set_home_dir(_home: &str) {
    // TODO: implement home dir change
}

pub fn main_start_dbus_server() {
    // not applicable on OHOS
}

pub fn main_start_ipc_url_server() {
    // TODO: implement IPC URL server
}

pub fn main_check_super_user_permission() -> bool {
    false
}

pub fn main_goto_install() -> String {
    String::new()
}

pub fn main_update_me(_path: &str) {
    // TODO: implement OHOS update mechanism
}

pub fn main_deploy_device() -> bool {
    false
}

pub fn main_get_main_display() -> String {
    String::new()
}

pub fn main_get_input_source() -> String {
    String::new()
}

pub fn main_set_input_source(_source: &str) {
    // TODO: implement input source
}

pub fn main_init_input_source() {
    // TODO: implement input source init
}

pub fn main_supported_input_source() -> String {
    "[]".to_owned()
}

pub fn main_video_save_directory() -> String {
    String::new()
}

pub fn main_get_data_dir_ios() -> String {
    String::new()
}

pub fn main_show_option(_key: &str) -> bool {
    true
}

pub fn main_set_options(_options: &str) {
    // TODO: parse json and set each option
}

pub fn main_get_options_sync() -> String {
    json!(config::Config::get_options()).to_string()
}

pub fn main_get_option_sync(key: &str) -> String {
    config::Config::get_option(key)
}

pub fn main_get_common_sync(key: &str) -> String {
    config::Config::get_option(key)
}

pub fn main_get_http_status() -> String {
    String::new()
}

pub fn main_uri_prefix_sync() -> String {
    String::new()
}

pub fn main_load_ab() -> String {
    // TODO: implement address book loading
    String::new()
}

pub fn main_save_ab(_ab: &str) {
    // TODO: implement address book saving
}

pub fn main_clear_ab() {
    // TODO: implement address book clearing
}

pub fn main_load_group() -> String {
    // TODO: implement group loading
    String::new()
}

pub fn main_save_group(_group: &str) {
    // TODO: implement group saving
}

pub fn main_clear_group() {
    // TODO: implement group clearing
}

pub fn main_load_fav_peers() -> String {
    String::new()
}

pub fn main_load_recent_peers_for_ab() -> String {
    String::new()
}

pub fn main_handle_wayland_screencast_restore_token(_token: &str) {
    // not applicable on OHOS (Wayland only)
}

// ============================================================
// misc missing functions
// ============================================================

pub fn get_double_click_time() -> f64 {
    0.3
}

pub fn get_local_flutter_option(k: &str) -> String {
    LocalConfig::get_option(k)
}

pub fn set_local_flutter_option(k: &str, v: &str) {
    LocalConfig::set_option(k.to_owned(), v.to_owned());
}

pub fn get_local_kb_layout_type() -> String {
    String::new()
}

pub fn set_local_kb_layout_type(v: &str) {
    config::Config::set_option("kb-layout-type".to_owned(), v.to_owned());
}

pub fn get_voice_call_input_device() -> String {
    String::new()
}

pub fn set_voice_call_input_device(_device: &str) {
    // TODO: implement voice call input device
}

pub fn host_stop_system_key_propagate(_stop: bool) {
    // not applicable on OHOS
}

pub fn option_synced() -> bool {
    false
}

pub fn peer_get_sessions_count() -> usize {
    0
}

pub fn send_url_scheme(_url: &str) {
    // TODO: implement URL scheme handling
}

pub fn set_cur_session_id(_id: &str) {
    // TODO: implement session ID tracking
}

pub fn start_global_event_stream() {
    // TODO: implement global event stream
}

pub fn stop_global_event_stream() {
    // TODO: implement global event stream
}

pub fn translate(name: &str) -> String {
    // TODO: implement i18n translation
    name.to_owned()
}

pub fn version_to_number(_v: &str) -> i64 {
    // TODO: implement version parsing
    0
}

pub fn will_session_close_close_session() -> bool {
    false
}

pub fn get_next_texture_key() -> i32 {
    0
}

pub fn session_add_existed_sync(_is_sync: bool) -> bool {
    false
}

pub fn session_add_job(
    _id: i32,
    _path: String,
    _to: String,
    _file_num: i32,
    _include_hidden: bool,
    _is_remote: bool,
) {
}

pub fn session_add_sync(_is_sync: bool) {}

pub fn session_get_audit_guid() -> String {
    String::new()
}

pub fn session_get_audit_server_sync(_typ: String) -> String {
    String::new()
}

pub fn session_get_common(_key: String) -> String {
    String::new()
}

pub fn session_get_common_sync(_key: String) -> String {
    String::new()
}

pub fn session_get_conn_session_id() -> String {
    String::new()
}

pub fn session_get_displays_as_individual_windows() -> String {
    String::new()
}

pub fn session_get_edge_scroll_edge_thickness() -> i32 {
    0
}

pub fn session_get_last_audit_note() -> String {
    String::new()
}

pub fn session_get_rgba_size(_display: c_int) -> c_int {
    0
}

pub fn session_get_toggle_option_sync(_arg: String) -> bool {
    false
}

pub fn session_get_use_all_my_displays_for_the_remote_session() -> String {
    String::new()
}

pub fn session_handle_screenshot(_action: String) -> String {
    String::new()
}

pub fn session_is_multi_ui_session() -> bool {
    false
}

pub fn session_next_rgba(_display: c_int) {}

pub fn session_on_waiting_for_image_dialog_show() {}

pub fn session_printer_response(_id: i32, _path: String, _printer_name: String) {}

pub fn session_read_dir_to_remove_recursive(_id: i32, _path: String, _include_hidden: bool) {}

pub fn session_read_local_dir_sync(_path: String, _include_hidden: bool, _id: i32) -> String {
    String::new()
}

pub fn session_read_local_empty_dirs_recursive_sync(_id: i32, _path: String) -> String {
    String::new()
}

pub fn session_read_remote_empty_dirs_recursive_sync(_id: i32, _path: String) -> String {
    String::new()
}

pub fn session_register_gpu_texture(_display: c_int) -> c_int {
    0
}

pub fn session_register_pixelbuffer_texture(_display: c_int) -> c_int {
    0
}

pub fn session_remove_all_empty_dirs(_id: i32) {}

pub fn session_request_new_display_init_msgs(_display: c_int) {}

pub fn session_send_pointer(_msg: String) {}

pub fn session_set_audit_guid(_guid: String) {}

pub fn session_set_displays_as_individual_windows(_value: String) {}

pub fn session_set_edge_scroll_edge_thickness(_value: i32) {}

pub fn session_set_use_all_my_displays_for_the_remote_session(_value: String) {}

pub fn session_start_with_displays(_displays: String) -> bool {
    false
}
