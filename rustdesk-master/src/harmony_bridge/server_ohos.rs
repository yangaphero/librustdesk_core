use hbb_common::{
    allow_err,
    bail,
    config::{
        self, decode_permanent_password_h1_from_storage,
        decode_preset_password_h1_from_storage,
        local_permanent_password_storage_is_usable_for_auth,
        preset_permanent_password_storage_is_usable_for_auth, Config, CONNECT_TIMEOUT, RELAY_PORT,
    },
    log,
    message_proto::{option_message::BoolOption, *},
    password_security::{self as password, ApproveMode},
    protobuf::{Enum, Message as _},
    rendezvous_proto::*,
    sha2::{Digest, Sha256},
    socket_client,
    sodiumoxide::crypto::{box_, sign},
    timeout, tokio, ResultType, Stream,
};
use hbb_common::{
    bytes::Bytes,
    futures::{SinkExt, StreamExt},
    tokio::sync::mpsc,
    tokio::time::{self, Duration, Instant},
};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread::{self, JoinHandle};
use std::time as std_time;

use crate::ipc;

extern "C" {
    fn rustdesk_ohos_inject_mouse(mask: i32, x: i32, y: i32) -> i32;
    fn rustdesk_ohos_inject_key(
        control_key: i32,
        unicode_value: u32,
        down: i32,
        press: i32,
        modifiers: u32,
    ) -> i32;
}

fn input_modifier_mask(event: &KeyEvent) -> u32 {
    event.modifiers.iter().fold(0_u32, |mask, modifier| {
        mask | match modifier.enum_value() {
            Ok(ControlKey::Alt) | Ok(ControlKey::RAlt) => 1,
            Ok(ControlKey::Control) | Ok(ControlKey::RControl) => 2,
            Ok(ControlKey::Shift) | Ok(ControlKey::RShift) => 4,
            Ok(ControlKey::Meta) | Ok(ControlKey::RWin) => 8,
            _ => 0,
        }
    })
}

fn inject_key_event(event: &KeyEvent) -> (&'static str, i32) {
    let down = if event.down { 1 } else { 0 };
    let press = i32::from(event.press);
    let modifiers = input_modifier_mask(event);
    match &event.union {
        Some(key_event::Union::ControlKey(value)) => (
            "control",
            unsafe {
                rustdesk_ohos_inject_key(
                    value.value(),
                    0,
                    down,
                    press,
                    modifiers,
                )
            },
        ),
        Some(key_event::Union::Chr(value)) => (
            "position",
            unsafe {
                rustdesk_ohos_inject_key(-1, *value, down, press, modifiers)
            },
        ),
        Some(key_event::Union::Unicode(value)) => (
            "unicode",
            unsafe {
                rustdesk_ohos_inject_key(-1, *value, down, press, modifiers)
            },
        ),
        Some(key_event::Union::Seq(value)) => {
            let mut result = 0;
            for character in value.chars() {
                let current = unsafe {
                    rustdesk_ohos_inject_key(
                        -1,
                        character as u32,
                        1,
                        1,
                        modifiers,
                    )
                };
                if current != 0 {
                    result = current;
                    break;
                }
            }
            ("sequence", result)
        }
        Some(key_event::Union::Win2winHotkey(value)) => (
            "hotkey",
            unsafe {
                rustdesk_ohos_inject_key(
                    -1,
                    value & 0xffff,
                    down,
                    press,
                    modifiers,
                )
            },
        ),
        Some(_) => ("unsupported", 401),
        None => ("none", 401),
    }
}

// ============================================================
// Platform stubs
// ============================================================

pub mod wayland {
    pub fn init() {}

    pub fn common_get_error() -> String {
        String::new()
    }
}

pub mod input_service {
    pub const NAME_CURSOR: &str = "";
    pub const NAME_POS: &str = "";
    pub const NAME_WINDOW_FOCUS: &str = "";

    pub fn fix_key_down_timeout_at_exit() {}
}

pub mod audio_service {
    pub const NAME: &str = "audio";

    pub fn set_voice_call_input_device(_device: Option<String>, _set_if_present: bool) {}

    pub fn new() -> super::GenericService {
        let svc = super::EmptyExtraFieldService::new(NAME.to_owned(), false);
        super::GenericService::run(&svc.clone(), |_: super::EmptyExtraFieldService| Ok(()));
        svc.sp
    }
}

pub mod display_service {
    use std::sync::Arc;

    lazy_static::lazy_static! {
        pub static ref PRIMARY_DISPLAY_IDX: Arc<usize> = Arc::new(0);
    }

    pub fn new() -> super::GenericService {
        let svc = super::EmptyExtraFieldService::new("display".to_owned(), true);
        super::GenericService::run(&svc.clone(), |_: super::EmptyExtraFieldService| Ok(()));
        svc.sp
    }

    pub fn is_inited_msg() -> Option<hbb_common::message_proto::Message> {
        None
    }

    pub fn update_get_sync_displays_on_login() -> hbb_common::anyhow::Result<
        Vec<hbb_common::message_proto::DisplayInfo>,
    > {
        let (w, h) = match scrap::Display::primary() {
            Ok(d) => (d.width(), d.height()),
            Err(_) => (720, 1280),
        };
        Ok(vec![hbb_common::message_proto::DisplayInfo {
            x: 0,
            y: 0,
            width: w as i32,
            height: h as i32,
            name: "OHOS".into(),
            online: true,
            ..Default::default()
        }])
    }

    pub fn check_display_changed(
        _current: usize,
        _last_n: usize,
        _last_w: usize,
        _last_h: usize,
    ) -> bool {
        false
    }
}

pub mod clipboard_service {
    pub const NAME: &str = "clipboard";
    pub const FILE_NAME: &str = "clipboard_file";

    pub fn new(name: String) -> super::GenericService {
        let svc = super::EmptyExtraFieldService::new(name, false);
        super::GenericService::run(&svc.clone(), |_: super::EmptyExtraFieldService| Ok(()));
        svc.sp
    }
}

// ============================================================
// video_service
// ============================================================

pub mod video_service {
    use super::*;
    use scrap::{Frame, TraitCapturer};

    pub const OPTION_REFRESH: &str = "refresh";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum VideoSource {
        Monitor,
        Camera,
    }

    impl VideoSource {
        pub fn service_name_prefix(&self) -> &'static str {
            match self {
                VideoSource::Monitor => "monitor",
                VideoSource::Camera => "camera",
            }
        }

        pub fn is_monitor(&self) -> bool {
            matches!(self, VideoSource::Monitor)
        }
    }

    pub fn get_service_name(source: VideoSource, idx: usize) -> String {
        format!("{}{}", source.service_name_prefix(), idx)
    }

    #[derive(Clone)]
    struct VideoService {
        sp: GenericService,
        idx: usize,
        source: VideoSource,
    }

    impl Deref for VideoService {
        type Target = ServiceTmpl<ConnInner>;

        fn deref(&self) -> &Self::Target {
            &self.sp
        }
    }

    impl DerefMut for VideoService {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.sp
        }
    }

    pub fn new(source: VideoSource, idx: usize) -> GenericService {
        let vs = VideoService {
            sp: GenericService::new(get_service_name(source, idx), true),
            idx,
            source,
        };
        GenericService::run(&vs, run);
        vs.sp
    }

    fn run(vs: VideoService) -> ResultType<()> {
        let display_idx = vs.idx;
        let sp = vs.sp;
        let name = sp.name();

        log::info!("OHOS video_service::run starting for {} display_idx={}", name, display_idx);

        let mut c = get_capturer(vs.source, display_idx)?;

        let quality = scrap::codec::Quality::default().ratio();
        let use_i444 = false;
        let encoder_cfg = scrap::codec::EncoderCfg::VPX(scrap::vpxcodec::VpxEncoderConfig {
            width: c.width as _,
            height: c.height as _,
            quality,
            codec: scrap::vpxcodec::VpxVideoCodecId::VP9,
            keyframe_interval: None,
        });
        scrap::codec::Encoder::set_fallback(&encoder_cfg);
        let mut encoder = scrap::codec::Encoder::new(encoder_cfg, use_i444)?;

        log::info!(
            "OHOS video_service encoder created: {}x{}, codec={:?}",
            c.width,
            c.height,
            scrap::codec::Encoder::negotiated_codec()
        );

        if sp.is_option_true(OPTION_REFRESH) {
            sp.set_option_bool(OPTION_REFRESH, false);
        }

        let start = std_time::Instant::now();
        let mut yuv = Vec::new();
        let mut mid_data = Vec::new();
        let mut first_frame = true;
        let mut repeat_encode_counter = 0;
        let repeat_encode_max = 10;
        let mut encode_fail_counter = 0;
        let capture_width = c.width;
        let capture_height = c.height;

        while sp.ok() {
            if sp.is_option_true(OPTION_REFRESH) {
                log::info!("OHOS video_service refresh requested, bailing for restart");
                bail!("SWITCH");
            }

            let time = start.elapsed();
            let ms = (time.as_secs() * 1000 + time.subsec_millis() as u64) as i64;

            let res: std::io::Result<Frame<'_>> = c.frame(std_time::Duration::from_millis(33));
            let res = match res {
                Ok(frame) => {
                    repeat_encode_counter = 0;
                    if frame.valid() {
                        let frame = frame.to(encoder.yuvfmt(), &mut yuv, &mut mid_data)?;
                        let send_conn_ids = handle_one_frame(
                            display_idx,
                            &sp,
                            frame,
                            ms,
                            &mut encoder,
                            &mut encode_fail_counter,
                            &mut first_frame,
                            capture_width,
                            capture_height,
                        )?;
                        send_conn_ids
                    } else {
                        Default::default()
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if !encoder.latency_free() && !yuv.is_empty() {
                        if repeat_encode_counter < repeat_encode_max {
                            repeat_encode_counter += 1;
                            handle_one_frame(
                                display_idx,
                                &sp,
                                scrap::EncodeInput::YUV(&yuv),
                                ms,
                                &mut encoder,
                                &mut encode_fail_counter,
                                &mut first_frame,
                                capture_width,
                                capture_height,
                            )?;
                        }
                    }
                    Default::default()
                }
                Err(err) => {
                    return Err(err.into());
                }
            };

            let elapsed = start.elapsed();
            let spf = std_time::Duration::from_millis(33);
            if elapsed < spf {
                std::thread::sleep(spf - elapsed);
            }
        }

        log::info!("OHOS video_service::run exiting for {}", name);
        Ok(())
    }

    fn handle_one_frame(
        display_idx: usize,
        sp: &GenericService,
        frame: scrap::EncodeInput<'_>,
        ms: i64,
        encoder: &mut scrap::codec::Encoder,
        encode_fail_counter: &mut usize,
        first_frame: &mut bool,
        capture_width: usize,
        capture_height: usize,
    ) -> ResultType<HashSet<i32>> {
        sp.snapshot(|new_subscribers| {
            // Match the official video service: move a lone first subscriber into
            // the active set, or restart the encoder when joining an existing stream.
            if new_subscribers.has_subscribes() {
                log::info!("OHOS video_service switching for new subscriber");
                bail!("SWITCH");
            }
            Ok(())
        })?;

        match encoder.encode_to_message(frame, ms) {
            Ok(mut vf) => {
                *encode_fail_counter = 0;
                vf.display = display_idx as _;
                if *first_frame {
                    *first_frame = false;
                    log::info!(
                        "OHOS video_service first frame encoded: {}x{}",
                        capture_width,
                        capture_height
                    );
                    crate::harmony_bridge::core::set_incoming_service_started(true);
                }
                let mut msg = Message::new();
                msg.set_video_frame(vf);
                Ok(sp.send_video_frame(msg))
            }
            Err(err) => {
                *encode_fail_counter += 1;
                if *encode_fail_counter > 10 {
                    log::error!("OHOS video_service encode failed 10 times: {}", err);
                    *encode_fail_counter = 0;
                }
                Ok(Default::default())
            }
        }
    }

    struct CapturerInfo {
        width: usize,
        height: usize,
        capturer: scrap::Capturer,
    }

    impl std::ops::Deref for CapturerInfo {
        type Target = scrap::Capturer;

        fn deref(&self) -> &Self::Target {
            &self.capturer
        }
    }

    impl std::ops::DerefMut for CapturerInfo {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.capturer
        }
    }

    fn get_capturer(source: VideoSource, display_idx: usize) -> ResultType<CapturerInfo> {
        let display = scrap::Display::primary()?;
        let width = display.width();
        let height = display.height();
        let capturer = scrap::Capturer::new(display)?;
        log::info!(
            "OHOS video_service get_capturer: source={:?} display_idx={} {}x{}",
            source,
            display_idx,
            width,
            height
        );
        Ok(CapturerInfo {
            width,
            height,
            capturer,
        })
    }

    lazy_static::lazy_static! {
        pub static ref VIDEO_QOS: Arc<Mutex<VideoQoS>> = Default::default();
    }

    pub struct VideoQoS {
        bitrate: u32,
    }

    impl Default for VideoQoS {
        fn default() -> Self {
            Self { bitrate: 0 }
        }
    }

    impl VideoQoS {
        pub fn bitrate(&self) -> u32 {
            self.bitrate
        }
    }
}

// ============================================================
// Service trait / ServiceTmpl / Subscriber
// ============================================================

pub trait Service: Send + Sync {
    fn name(&self) -> String;
    fn on_subscribe(&self, sub: ConnInner);
    fn on_unsubscribe(&self, id: i32);
    fn is_subed(&self, id: i32) -> bool;
    fn join(&self);
    fn get_option(&self, opt: &str) -> Option<String>;
    fn set_option(&self, opt: &str, val: &str) -> Option<String>;
    fn ok(&self) -> bool;
}

pub trait Subscriber: Default + Send + Sync + 'static {
    fn id(&self) -> i32;
    fn send(&mut self, msg: Arc<Message>);
}

#[derive(Default)]
pub struct ServiceInner<T: Subscriber + From<ConnInner>> {
    name: String,
    handle: Option<JoinHandle<()>>,
    subscribes: HashMap<i32, T>,
    new_subscribes: HashMap<i32, T>,
    active: bool,
    need_snapshot: bool,
    options: HashMap<String, String>,
}

pub trait Reset {
    fn reset(&mut self);
    fn init(&mut self) {}
}

pub struct ServiceTmpl<T: Subscriber + From<ConnInner>>(Arc<RwLock<ServiceInner<T>>>);
pub struct ServiceSwap<T: Subscriber + From<ConnInner>>(ServiceTmpl<T>);
pub type GenericService = ServiceTmpl<ConnInner>;
pub const HIBERNATE_TIMEOUT: u64 = 30;
pub const MAX_ERROR_TIMEOUT: u64 = 1_000;
pub const SERVICE_OPTION_VALUE_TRUE: &str = "1";
pub const SERVICE_OPTION_VALUE_FALSE: &str = "0";

#[derive(Clone)]
pub struct EmptyExtraFieldService {
    pub sp: GenericService,
}

impl Deref for EmptyExtraFieldService {
    type Target = ServiceTmpl<ConnInner>;

    fn deref(&self) -> &Self::Target {
        &self.sp
    }
}

impl DerefMut for EmptyExtraFieldService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sp
    }
}

impl EmptyExtraFieldService {
    pub fn new(name: String, need_snapshot: bool) -> Self {
        Self {
            sp: GenericService::new(name, need_snapshot),
        }
    }
}

impl<T: Subscriber + From<ConnInner>> ServiceInner<T> {
    fn send_new_subscribes(&mut self, msg: Arc<Message>) {
        for s in self.new_subscribes.values_mut() {
            s.send(msg.clone());
        }
    }

    fn swap_new_subscribes(&mut self) {
        for (_, s) in self.new_subscribes.drain() {
            self.subscribes.insert(s.id(), s);
        }
        debug_assert!(self.new_subscribes.is_empty());
    }

    #[inline]
    fn has_subscribes(&self) -> bool {
        self.subscribes.len() > 0 || self.new_subscribes.len() > 0
    }
}

impl<T: Subscriber + From<ConnInner>> Service for ServiceTmpl<T> {
    #[inline]
    fn name(&self) -> String {
        self.0.read().unwrap().name.clone()
    }

    fn is_subed(&self, id: i32) -> bool {
        self.0.read().unwrap().subscribes.get(&id).is_some()
            || self.0.read().unwrap().new_subscribes.get(&id).is_some()
    }

    fn on_subscribe(&self, sub: ConnInner) {
        let mut lock = self.0.write().unwrap();
        if lock.subscribes.get(&sub.id()).is_some() {
            return;
        }
        if lock.need_snapshot {
            lock.new_subscribes.insert(sub.id(), sub.into());
        } else {
            lock.subscribes.insert(sub.id(), sub.into());
        }
    }

    fn on_unsubscribe(&self, id: i32) {
        let mut lock = self.0.write().unwrap();
        if lock.subscribes.remove(&id).is_none() {
            lock.new_subscribes.remove(&id);
        }
    }

    fn join(&self) {
        self.0.write().unwrap().active = false;
        let handle = self.0.write().unwrap().handle.take();
        if let Some(handle) = handle {
            if let Err(e) = handle.join() {
                log::error!("Failed to join thread for service {}, {:?}", self.name(), e);
            }
        }
    }

    fn get_option(&self, opt: &str) -> Option<String> {
        self.0.read().unwrap().options.get(opt).cloned()
    }

    fn set_option(&self, opt: &str, val: &str) -> Option<String> {
        self.0
            .write()
            .unwrap()
            .options
            .insert(opt.to_string(), val.to_string())
    }

    #[inline]
    fn ok(&self) -> bool {
        let lock = self.0.read().unwrap();
        lock.active && lock.has_subscribes()
    }
}

impl<T: Subscriber + From<ConnInner>> Clone for ServiceTmpl<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Subscriber + From<ConnInner>> ServiceTmpl<T> {
    pub fn new(name: String, need_snapshot: bool) -> Self {
        Self(Arc::new(RwLock::new(ServiceInner::<T> {
            name,
            active: true,
            need_snapshot,
            ..Default::default()
        })))
    }

    #[inline]
    pub fn is_option_true(&self, opt: &str) -> bool {
        self.get_option(opt)
            .map_or(false, |v| v == SERVICE_OPTION_VALUE_TRUE)
    }

    #[inline]
    pub fn set_option_bool(&self, opt: &str, val: bool) {
        if val {
            self.set_option(opt, SERVICE_OPTION_VALUE_TRUE);
        } else {
            self.set_option(opt, SERVICE_OPTION_VALUE_FALSE);
        }
    }

    #[inline]
    pub fn has_subscribes(&self) -> bool {
        self.0.read().unwrap().has_subscribes()
    }

    pub fn snapshot<F>(&self, callback: F) -> ResultType<()>
    where
        F: FnMut(ServiceSwap<T>) -> ResultType<()>,
    {
        if self.0.read().unwrap().new_subscribes.len() > 0 {
            log::info!("Call snapshot of {} service", self.name());
            let mut callback = callback;
            callback(ServiceSwap::<T>(self.clone()))?;
        }
        Ok(())
    }

    #[inline]
    pub fn send(&self, msg: Message) {
        self.send_shared(Arc::new(msg));
    }

    pub fn send_to(&self, msg: Message, id: i32) {
        if let Some(s) = self.0.write().unwrap().subscribes.get_mut(&id) {
            s.send(Arc::new(msg));
        }
    }

    pub fn send_shared(&self, msg: Arc<Message>) {
        let mut lock = self.0.write().unwrap();
        for s in lock.subscribes.values_mut() {
            s.send(msg.clone());
        }
    }

    pub fn send_video_frame(&self, msg: Message) -> HashSet<i32> {
        let msg = Arc::new(msg);
        let mut conn_ids = HashSet::new();
        let mut lock = self.0.write().unwrap();
        for s in lock.subscribes.values_mut() {
            s.send(msg.clone());
            conn_ids.insert(s.id());
        }
        conn_ids
    }

    pub fn run<F, Svc>(svc: &Svc, callback: F)
    where
        F: FnMut(Svc) -> ResultType<()> + Send + 'static,
        Svc: Clone + Send + DerefMut<Target = ServiceTmpl<T>> + 'static,
    {
        let sp = svc.clone();
        let mut callback = callback;
        let handle = thread::spawn(move || {
            let mut error_timeout = HIBERNATE_TIMEOUT;
            while sp.active() {
                if sp.has_subscribes() {
                    log::debug!("Enter {} service inner loop", sp.name());
                    let tm = std_time::Instant::now();
                    if let Err(err) = callback(sp.clone()) {
                        log::error!("Error of {} service: {}", sp.name(), err);
                        if tm.elapsed() > std_time::Duration::from_millis(MAX_ERROR_TIMEOUT) {
                            error_timeout = HIBERNATE_TIMEOUT;
                        } else {
                            error_timeout *= 2;
                        }
                        if error_timeout > MAX_ERROR_TIMEOUT {
                            error_timeout = MAX_ERROR_TIMEOUT;
                        }
                        thread::sleep(std_time::Duration::from_millis(error_timeout));
                    } else {
                        log::debug!("Exit {} service inner loop", sp.name());
                    }
                }
                thread::sleep(std_time::Duration::from_millis(HIBERNATE_TIMEOUT));
            }
            log::info!("Service {} exit", sp.name());
        });
        svc.0.write().unwrap().handle = Some(handle);
    }

    #[inline]
    pub fn active(&self) -> bool {
        self.0.read().unwrap().active
    }
}

impl<T: Subscriber + From<ConnInner>> ServiceSwap<T> {
    #[inline]
    pub fn send(&self, msg: Message) {
        self.send_shared(Arc::new(msg));
    }

    #[inline]
    pub fn send_shared(&self, msg: Arc<Message>) {
        (self.0).0.write().unwrap().send_new_subscribes(msg);
    }

    #[inline]
    pub fn has_subscribes(&self) -> bool {
        (self.0).0.read().unwrap().subscribes.len() > 0
    }
}

impl<T: Subscriber + From<ConnInner>> Drop for ServiceSwap<T> {
    fn drop(&mut self) {
        (self.0).0.write().unwrap().swap_new_subscribes();
    }
}

// ============================================================
// Sender / ConnInner / Subscriber impl
// ============================================================

pub type Sender = mpsc::UnboundedSender<(Instant, Arc<Message>)>;

#[derive(Clone, Default)]
pub struct ConnInner {
    id: i32,
    tx: Option<Sender>,
    tx_video: Option<Sender>,
}

impl ConnInner {
    pub fn new(id: i32, tx: Option<Sender>, tx_video: Option<Sender>) -> Self {
        Self { id, tx, tx_video }
    }

    pub fn id(&self) -> i32 {
        self.id
    }
}

impl Subscriber for ConnInner {
    #[inline]
    fn id(&self) -> i32 {
        self.id
    }

    #[inline]
    fn send(&mut self, msg: Arc<Message>) {
        let tx_by_video = match &msg.union {
            Some(message::Union::VideoFrame(_)) => true,
            Some(message::Union::Misc(misc)) => match &misc.union {
                Some(misc::Union::SwitchDisplay(_)) => true,
                _ => false,
            },
            _ => false,
        };
        let tx = if tx_by_video {
            self.tx_video.as_mut()
        } else {
            self.tx.as_mut()
        };
        tx.map(|tx| {
            allow_err!(tx.send((Instant::now(), msg)));
        });
    }
}

// ============================================================
// Server
// ============================================================

type ConnMap = HashMap<i32, ConnInner>;

pub struct Server {
    connections: ConnMap,
    services: HashMap<String, Box<dyn Service>>,
    id_count: i32,
}

pub type ServerPtr = Arc<RwLock<Server>>;
pub type ServerPtrWeak = Weak<RwLock<Server>>;

pub fn new() -> ServerPtr {
    let mut server = Server {
        connections: HashMap::new(),
        services: HashMap::new(),
        id_count: hbb_common::rand::random::<i32>() % 1000 + 1000,
    };
    server.add_service(Box::new(audio_service::new()));
    server.add_service(Box::new(display_service::new()));
    server.add_service(Box::new(clipboard_service::new(clipboard_service::NAME.to_owned())));
    Arc::new(RwLock::new(server))
}

impl Server {
    pub fn new() -> Self {
        Server {
            connections: ConnMap::new(),
            services: HashMap::new(),
            id_count: hbb_common::rand::random::<i32>() % 1000 + 1000,
        }
    }

    pub fn get_new_id(&mut self) -> i32 {
        self.id_count += 1;
        self.id_count
    }

    pub fn subscribe(&mut self, name: &str, conn: ConnInner, sub: bool) {
        if let Some(s) = self.services.get(name) {
            if s.is_subed(conn.id()) == sub {
                return;
            }
            if sub {
                s.on_subscribe(conn.clone());
            } else {
                s.on_unsubscribe(conn.id());
            }
        }
    }

    pub fn add_connection(&mut self, conn: ConnInner, noperms: &Vec<&'static str>) {
        let primary_video_service_name =
            video_service::get_service_name(video_service::VideoSource::Monitor, 0);
        for s in self.services.values() {
            let name = s.name();
            if Self::is_video_service_name(&name) && name != primary_video_service_name {
                continue;
            }
            if !noperms.contains(&(&name as _)) {
                s.on_subscribe(conn.clone());
            }
        }
        self.connections.insert(conn.id(), conn);
    }

    pub fn remove_connection(&mut self, conn: &ConnInner) {
        for s in self.services.values() {
            s.on_unsubscribe(conn.id());
        }
        self.connections.remove(&conn.id());
    }

    pub fn close_connections(&mut self) {
        let conn_inners: Vec<_> = self.connections.values_mut().collect();
        for c in conn_inners {
            let mut misc = Misc::new();
            misc.set_stop_service(true);
            let mut msg = Message::new();
            msg.set_misc(misc);
            c.send(Arc::new(msg));
        }
    }

    fn add_service(&mut self, service: Box<dyn Service>) {
        let name = service.name();
        self.services.insert(name, service);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.services.contains_key(name)
    }

    fn is_video_service_name(name: &str) -> bool {
        name.starts_with(video_service::VideoSource::Monitor.service_name_prefix())
            || name.starts_with(video_service::VideoSource::Camera.service_name_prefix())
    }

    pub fn try_add_primay_video_service(&mut self) {
        let primary_video_service_name =
            video_service::get_service_name(video_service::VideoSource::Monitor, 0);
        if !self.contains(&primary_video_service_name) {
            self.add_service(Box::new(video_service::new(
                video_service::VideoSource::Monitor,
                0,
            )));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        for s in self.services.values() {
            s.join();
        }
    }
}

lazy_static::lazy_static! {
    pub static ref CLIENT_SERVER: ServerPtr = new();
}

// ============================================================
// Connection
// ============================================================

lazy_static::lazy_static! {
    static ref ALIVE_CONNS: Arc<Mutex<Vec<i32>>> = Default::default();
    static ref PEER_CONNS: Arc<Mutex<HashMap<String, i32>>> = Default::default();
    static ref PENDING_APPROVALS: Arc<Mutex<HashMap<String, PendingApproval>>> = Default::default();
    static ref APPROVED_PEERS: Arc<Mutex<HashMap<String, std_time::Instant>>> = Default::default();
    static ref REJECTED_PEERS: Arc<Mutex<HashMap<String, std_time::Instant>>> = Default::default();
}

const APPROVAL_TOKEN_TTL: std_time::Duration = std_time::Duration::from_secs(60);

pub struct PendingApproval {
    pub conn_id: i32,
    pub name: String,
    pub conn_type: String,
    pub version: String,
    pub requested_at: std_time::Instant,
}

pub fn needs_click_approval() -> bool {
    let approve_mode = password::approve_mode();
    approve_mode == ApproveMode::Click
        || (approve_mode == ApproveMode::Both && !password::has_valid_password())
}

/// Called on each LoginRequest when click approval is required.
/// Returns:
///   0 -> keep waiting (approval pending; a NO_PASSWORD_ACCESS error was already sent)
///   1 -> approved (consume token; caller proceeds with normal authorization)
///   2 -> rejected (caller should send error and close the connection)
pub fn check_click_approval(id: i32, peer_id: &str, peer_name: &str, conn_type: &str, version: &str) -> i32 {
    // 1) one-time approval token from a previous Accept action
    if let Some(at) = APPROVED_PEERS.lock().unwrap().remove(peer_id) {
        if at.elapsed() < APPROVAL_TOKEN_TTL {
            return 1;
        }
    }
    // 2) fresh rejection window: reject immediately
    {
        let rejected = REJECTED_PEERS.lock().unwrap();
        if let Some(at) = rejected.get(peer_id) {
            if at.elapsed() < APPROVAL_TOKEN_TTL {
                return 2;
            }
        }
    }
    // 3) register/reuse pending entry and ask the UI again on the first request only
    {
        let mut pending = PENDING_APPROVALS.lock().unwrap();
        match pending.get(peer_id) {
            Some(existing) if existing.requested_at.elapsed() < APPROVAL_TOKEN_TTL => {
                return 0;
            }
            _ => {}
        }
        pending.insert(
            peer_id.to_owned(),
            PendingApproval {
                conn_id: id,
                name: peer_name.to_owned(),
                conn_type: conn_type.to_owned(),
                version: version.to_owned(),
                requested_at: std_time::Instant::now(),
            },
        );
    }
    REJECTED_PEERS.lock().unwrap().remove(peer_id);
    crate::harmony_bridge::core::queue_event(
        "connection-request",
        &format!(
            "id={};name={};type={};version={}",
            peer_id, peer_name, conn_type, version
        ),
        peer_id,
    );
    log::info!(
        "OHOS Connection #{} waiting for user approval: peer={} name={} type={}",
        id,
        peer_id,
        peer_name,
        conn_type
    );
    0
}

/// JS entry: respond to a pending incoming-connection request.
pub fn respond_connection_request(peer_id: &str, accept: bool) -> bool {
    PENDING_APPROVALS.lock().unwrap().remove(peer_id);
    if accept {
        REJECTED_PEERS.lock().unwrap().remove(peer_id);
        APPROVED_PEERS
            .lock()
            .unwrap()
            .insert(peer_id.to_owned(), std_time::Instant::now());
    } else {
        APPROVED_PEERS.lock().unwrap().remove(peer_id);
        REJECTED_PEERS
            .lock()
            .unwrap()
            .insert(peer_id.to_owned(), std_time::Instant::now());
    }
    log::info!(
        "OHOS connection approval decision: peer={} accept={}",
        peer_id,
        accept
    );
    true
}

const TEST_DELAY_TIMEOUT: Duration = Duration::from_secs(1);
const SEC30: Duration = Duration::from_secs(60);
const SEND_TIMEOUT_VIDEO: u64 = 30_000;
const SEND_TIMEOUT_OTHER: u64 = SEND_TIMEOUT_VIDEO * 10;

#[derive(serde_derive::Serialize)]
pub struct Connection;

impl Connection {
    pub fn alive_conns() -> Vec<Connection> {
        Vec::new()
    }

    pub async fn start(
        addr: SocketAddr,
        stream: Stream,
        id: i32,
        server: ServerPtrWeak,
        _control_permissions: Option<ControlPermissions>,
    ) {
        let addr = hbb_common::try_into_v4(addr);
        log::info!("OHOS Connection::start #{} from {}", id, addr);

        ALIVE_CONNS.lock().unwrap().push(id);

        let salt = Config::get_effective_permanent_password_salt();
        let hash = Hash {
            salt,
            challenge: Config::get_auto_password(6),
            ..Default::default()
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Arc<Message>)>();
        let (tx_video, mut rx_video) = mpsc::unbounded_channel::<(Instant, Arc<Message>)>();
        let (tx_to_cm, _rx_from_cm) = mpsc::unbounded_channel::<ipc::Data>();

        let inner = ConnInner {
            id,
            tx: Some(tx),
            tx_video: Some(tx_video),
        };

        let mut stream = stream;
        let mut authorized = false;
        let mut keyboard = true;
        let mut clipboard = true;
        let mut audio = true;
        let mut lr: LoginRequest = Default::default();
        let mut services_subed = false;

        let mut msg_out = Message::new();
        msg_out.set_hash(hash.clone());
        if let Err(e) = stream.send(&msg_out).await {
            log::error!("OHOS Connection::start #{} failed to send hash: {}", id, e);
            cleanup_connection(id, &server, &inner);
            return;
        }

        log::info!("OHOS Connection::start #{} hash sent, waiting for login", id);

        let mut test_delay_timer =
            crate::rustdesk_interval(time::interval_at(Instant::now(), TEST_DELAY_TIMEOUT));
        let mut last_recv_time = Instant::now();

        stream.set_send_timeout(SEND_TIMEOUT_VIDEO);

        let mut video_send_fail_count: u32 = 0;

        loop {
            tokio::select! {
                res = stream.next() => {
                    if let Some(res) = res {
                        match res {
                            Err(err) => {
                                log::warn!("OHOS Connection #{} stream error: {}", id, err);
                                break;
                            }
                            Ok(bytes) => {
                                last_recv_time = Instant::now();
                                if let Ok(msg_in) = Message::parse_from_bytes(&bytes) {
                                    if !handle_incoming_message(
                                        &mut stream,
                                        &inner,
                                        &server,
                                        &mut authorized,
                                        &mut keyboard,
                                        &mut clipboard,
                                        &mut audio,
                                        &mut lr,
                                        &mut services_subed,
                                        id,
                                        &hash,
                                        msg_in,
                                    ).await {
                                        break;
                                    }
                                }
                            }
                        }
                    } else {
                        log::info!("OHOS Connection #{} stream closed by peer", id);
                        break;
                    }
                }
                Some((_instant, value)) = rx_video.recv() => {
                    if let Err(err) = stream.send(&value as &Message).await {
                        video_send_fail_count += 1;
                        if video_send_fail_count > 5 {
                            log::warn!("OHOS Connection #{} video send error after 5 retries: {}", id, err);
                            break;
                        }
                        log::warn!("OHOS Connection #{} video send error (retry {}/5): {}", id, video_send_fail_count, err);
                    } else {
                        video_send_fail_count = 0;
                    }
                }
                Some((_instant, value)) = rx.recv() => {
                    let msg: &Message = &value;
                    if let Err(err) = stream.send(msg).await {
                        log::warn!("OHOS Connection #{} send error: {}", id, err);
                        break;
                    }
                }
                _ = test_delay_timer.tick() => {
                    if last_recv_time.elapsed() >= SEC30 {
                        log::warn!("OHOS Connection #{} timeout", id);
                        break;
                    }
                }
            }
        }

        cleanup_connection(id, &server, &inner);
        log::info!("#{} OHOS connection loop exited", id);
    }
}

async fn handle_incoming_message(
    stream: &mut Stream,
    inner: &ConnInner,
    server: &ServerPtrWeak,
    authorized: &mut bool,
    keyboard: &mut bool,
    clipboard: &mut bool,
    audio: &mut bool,
    lr: &mut LoginRequest,
    services_subed: &mut bool,
    id: i32,
    hash: &Hash,
    msg_in: Message,
) -> bool {
    match msg_in.union {
        Some(message::Union::LoginRequest(login_req)) => {
            *lr = login_req;
            log::info!(
                "OHOS Connection #{} LoginRequest from {} (id={})",
                id,
                lr.my_name,
                lr.my_id
            );

            let is_file_transfer = lr.union == Some(login_request::Union::FileTransfer(Default::default()));

            if !validate_password(hash, lr) {
                let mut res = LoginResponse::new();
                res.set_error(crate::client::LOGIN_MSG_PASSWORD_WRONG.to_owned());
                let mut msg_out = Message::new();
                msg_out.set_login_response(res);
                stream.send(&msg_out).await.ok();
                crate::harmony_bridge::core::queue_event(
                    "login-failed",
                    &format!("id={} wrong password", lr.my_id),
                    &lr.my_id,
                );
                // Keep the original socket alive so the controlling client can
                // display its password prompt and submit another LoginRequest.
                // Closing here turns an ordinary empty/wrong-password challenge
                // into "Connection reset by peer" before the prompt is usable.
                return true;
            }

            // Click approval: mirror the official flow. The client receives
            // "No Password Access" (it then shows a waiting dialog and retries
            // the LoginRequest automatically); once the local user accepts, the
            // retry is authorized through a one-time approval token.
            if needs_click_approval() {
                let conn_type_str = if is_file_transfer { "file-transfer" } else { "remote-control" };
                match check_click_approval(id, &lr.my_id, &lr.my_name, conn_type_str, &lr.version) {
                    0 => {
                        let mut res = LoginResponse::new();
                        res.set_error(crate::client::LOGIN_MSG_NO_PASSWORD_ACCESS.to_owned());
                        let mut msg_out = Message::new();
                        msg_out.set_login_response(res);
                        stream.send(&msg_out).await.ok();
                        return true;
                    }
                    2 => {
                        let mut res = LoginResponse::new();
                        res.set_error("Connection rejected by the peer".to_owned());
                        let mut msg_out = Message::new();
                        msg_out.set_login_response(res);
                        stream.send(&msg_out).await.ok();
                        crate::harmony_bridge::core::queue_event(
                            "login-failed",
                            &format!("id={} rejected by user", lr.my_id),
                            &lr.my_id,
                        );
                        return false;
                    }
                    _ => {}
                }
            }

            *authorized = true;

            let peer_id = lr.my_id.clone();
            if let Some(old_id) = PEER_CONNS.lock().unwrap().insert(peer_id.clone(), id) {
                if old_id != id && ALIVE_CONNS.lock().unwrap().contains(&old_id) {
                    log::info!(
                        "OHOS Connection #{} replacing old connection #{} from peer {}",
                        id, old_id, peer_id
                    );
                    if let Some(s) = server.upgrade() {
                        let s = s.read().unwrap();
                        if let Some(old_conn) = s.connections.get(&old_id) {
                            if let Some(tx) = &old_conn.tx {
                                let mut misc = Misc::new();
                                misc.set_close_reason("Replaced by new connection".to_string());
                                let mut msg_out = Message::new();
                                msg_out.set_misc(misc);
                                let _ = tx.send((Instant::now(), Arc::new(msg_out)));
                            }
                        }
                    }
                    ALIVE_CONNS.lock().unwrap().retain(|&x| x != old_id);
                    if let Some(s) = server.upgrade() {
                        s.write().unwrap().connections.remove(&old_id);
                    }
                }
            }

            let mut res = LoginResponse::new();
            let mut pi = PeerInfo {
                username: crate::platform::get_active_username(),
                version: crate::VERSION.to_owned(),
                hostname: crate::whoami_hostname(),
                platform: "OHOS".into(),
                ..Default::default()
            };

            let supported_encoding = scrap::codec::Encoder::supported_encoding();
            pi.encoding = Some(supported_encoding).into();

            match display_service::update_get_sync_displays_on_login() {
                Ok(displays) => {
                    pi.displays = displays;
                    pi.current_display = 0;
                }
                Err(err) => {
                    res.set_error(format!("{}", err));
                }
            }

            pi.features = Some(Features {
                privacy_mode: false,
                terminal: false,
                ..Default::default()
            })
            .into();

            res.set_peer_info(pi);
            let mut msg_out = Message::new();
            msg_out.set_login_response(res);
            stream.send(&msg_out).await.ok();

            crate::harmony_bridge::core::queue_event(
                "login-authorized",
                &format!("id={} name={}", lr.my_id, lr.my_name),
                &lr.my_id,
            );

            if !*services_subed && !is_file_transfer {
                *services_subed = true;
                if let Some(s) = server.upgrade() {
                    let mut noperms = Vec::new();
                    if !*audio {
                        noperms.push(audio_service::NAME);
                    }
                    let mut s = s.write().unwrap();
                    s.try_add_primay_video_service();
                    s.add_connection(inner.clone(), &noperms);
                }
            }

            true
        }
        Some(message::Union::MouseEvent(me)) => {
            if *keyboard {
                let result = unsafe { rustdesk_ohos_inject_mouse(me.mask, me.x, me.y) };
                crate::harmony_bridge::core::queue_event(
                    "mouse-input",
                    &format!(
                        "mask={};x={};y={};result={result}",
                        me.mask, me.x, me.y
                    ),
                    "",
                );
            }
            true
        }
        Some(message::Union::KeyEvent(me)) => {
            if *keyboard {
                let (control_key, unicode_value) = match &me.union {
                    Some(key_event::Union::ControlKey(value)) => (value.value(), 0),
                    Some(key_event::Union::Chr(value)) => (-1, *value),
                    _ => (-1, 0),
                };
                let modifiers = input_modifier_mask(&me);
                let (kind, result) = inject_key_event(&me);
                crate::harmony_bridge::core::queue_event(
                    "keyboard-input",
                    &format!(
                        "kind={kind};control={control_key};unicode={unicode_value};down={};press={};modifiers={modifiers};result={result}",
                        me.down, me.press,
                    ),
                    "",
                );
            }
            true
        }
        Some(message::Union::Cliprdr(_clip)) => {
            crate::harmony_bridge::core::queue_event("clipboard", "clip-from-peer", "");
            true
        }
        Some(message::Union::MultiClipboards(_mcs)) => {
            crate::harmony_bridge::core::queue_event("multi-clipboard", "multi-clip-from-peer", "");
            true
        }
        Some(message::Union::FileAction(_fa)) => {
            crate::harmony_bridge::core::queue_event("file-action", "file-action-from-peer", "");
            true
        }
        Some(message::Union::Misc(misc)) => match &misc.union {
            Some(misc::Union::ChatMessage(chat)) => {
                crate::harmony_bridge::core::queue_event(
                    "chat-message-incoming",
                    &chat.text,
                    "",
                );
                true
            }
            Some(misc::Union::SwitchDisplay(sd)) => {
                crate::harmony_bridge::core::queue_event(
                    "switch-display",
                    &format!("display={}", sd.display),
                    "",
                );
                true
            }
            Some(misc::Union::RefreshVideo(_)) => {
                if let Some(s) = server.upgrade() {
                    let name = video_service::get_service_name(video_service::VideoSource::Monitor, 0);
                    s.write().unwrap().subscribe(&name, inner.clone(), true);
                }
                true
            }
            Some(misc::Union::Option(opt)) => {
                handle_option_message(opt, keyboard, clipboard, audio, inner, server);
                true
            }
            Some(misc::Union::StopService(_)) => {
                log::info!("OHOS Connection #{} stop service requested", id);
                false
            }
            _ => true,
        }
        Some(message::Union::TestDelay(td)) => {
            let mut msg_out = Message::new();
            msg_out.set_test_delay(td);
            stream.send(&msg_out).await.ok();
            true
        }
        _ => true,
    }
}

fn handle_option_message(
    opt: &OptionMessage,
    keyboard: &mut bool,
    clipboard: &mut bool,
    audio: &mut bool,
    inner: &ConnInner,
    server: &ServerPtrWeak,
) {
    if let Ok(q) = opt.disable_keyboard.enum_value() {
        if q == BoolOption::Yes {
            *keyboard = false;
        }
    }
    if let Ok(q) = opt.disable_clipboard.enum_value() {
        if q == BoolOption::Yes {
            *clipboard = false;
        }
    }
    if let Ok(q) = opt.disable_audio.enum_value() {
        if q == BoolOption::Yes {
            *audio = false;
        }
    }
    if let Some(s) = server.upgrade() {
        if !*audio {
            s.write().unwrap().subscribe(audio_service::NAME, inner.clone(), false);
        }
    }
}

fn validate_password(hash: &Hash, lr: &LoginRequest) -> bool {
    if password::temporary_enabled() {
        let tmp_password = password::temporary_password();
        if validate_password_plain(hash, lr, &tmp_password) {
            return true;
        }
    }

    if password::permanent_enabled() {
        let (local_storage, local_salt) = Config::get_local_permanent_password_storage_and_salt();
        if !local_storage.is_empty() {
            if local_permanent_password_storage_is_usable_for_auth(&local_storage, &local_salt)
                && validate_password_storage(hash, lr, &local_storage)
            {
                return true;
            }
        } else {
            let (hard, salt) = Config::get_preset_password_storage_and_salt();
            if preset_permanent_password_storage_is_usable_for_auth(&hard, &salt)
                && validate_preset_password_storage(hash, lr, &hard, &salt)
            {
                return true;
            }
        }
    }

    if password::approve_mode() == ApproveMode::Click && lr.password.is_empty() {
        let perm = Config::get_option("permanent-password");
        if perm.is_empty() {
            return true;
        }
    }

    false
}

fn verify_h1(hash: &Hash, lr: &LoginRequest, h1: &[u8]) -> bool {
    if lr.password.len() < 32 {
        return false;
    }
    let mut hasher2 = Sha256::new();
    hasher2.update(h1);
    hasher2.update(hash.challenge.as_bytes());
    constant_time_eq(&hasher2.finalize()[..], &lr.password[..32])
}

fn validate_password_plain(hash: &Hash, lr: &LoginRequest, password: &str) -> bool {
    if password.is_empty() {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(hash.salt.as_bytes());
    let h1 = hasher.finalize();
    verify_h1(hash, lr, &h1[..])
}

fn validate_password_storage(hash: &Hash, lr: &LoginRequest, storage: &str) -> bool {
    if storage.is_empty() {
        return false;
    }
    if let Some(h1) = decode_permanent_password_h1_from_storage(storage) {
        return verify_h1(hash, lr, &h1[..]);
    }
    validate_password_plain(hash, lr, storage)
}

fn validate_preset_password_storage(hash: &Hash, lr: &LoginRequest, storage: &str, salt: &str) -> bool {
    if salt.is_empty() {
        return validate_password_plain(hash, lr, storage);
    }
    let Some(h1) = decode_preset_password_h1_from_storage(storage) else {
        return false;
    };
    verify_h1(hash, lr, &h1[..])
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut x: u8 = 0;
    for i in 0..a.len() {
        x |= a[i] ^ b[i];
    }
    x == 0
}

fn cleanup_connection(id: i32, server: &ServerPtrWeak, inner: &ConnInner) {
    ALIVE_CONNS.lock().unwrap().retain(|&x| x != id);
    PEER_CONNS.lock().unwrap().retain(|_, v| *v != id);
    PENDING_APPROVALS.lock().unwrap().retain(|_, p| p.conn_id != id);
    if let Some(s) = server.upgrade() {
        s.write().unwrap().remove_connection(inner);
    }
    crate::harmony_bridge::core::queue_event(
        "connection-closed",
        &format!("id={}", id),
        "",
    );
}

pub const CLICK_TIME: AtomicI64 = AtomicI64::new(0);
pub const MOUSE_MOVE_TIME: AtomicI64 = AtomicI64::new(0);

pub fn check_zombie() {}

pub async fn start_server(is_server: bool, _no_server: bool) {
    if is_server {
        crate::common::set_server_running(true);
        log::info!("OHOS server starting: set_server_running(true), starting RendezvousMediator");
        crate::harmony_bridge::core::queue_event(
            "server-starting",
            "OHOS incoming server starting",
            "",
        );
        crate::RendezvousMediator::start_all().await;
    } else {
        log::info!("OHOS server not starting (is_server=false)");
    }
}

pub async fn start_ipc_url_server() {}

pub async fn accept_connection(
    server: ServerPtr,
    socket: Stream,
    peer_addr: SocketAddr,
    secure: bool,
) {
    if let Err(err) = accept_connection_(server, socket, secure).await {
        log::warn!("Failed to accept connection from {}: {}", peer_addr, err);
    }
}

async fn accept_connection_(
    server: ServerPtr,
    socket: Stream,
    secure: bool,
) -> ResultType<()> {
    let local_addr = socket.local_addr();
    drop(socket);
    let listener = hbb_common::tcp::new_listener(local_addr, true).await?;
    log::info!("OHOS Server listening on: {}", &listener.local_addr()?);
    if let Ok((stream, addr)) = timeout(CONNECT_TIMEOUT, listener.accept()).await? {
        stream.set_nodelay(true).ok();
        let stream_addr = stream.local_addr()?;
        create_tcp_connection(
            server,
            Stream::from(stream, stream_addr),
            addr,
            secure,
            None,
        )
        .await?;
    }
    Ok(())
}

pub async fn create_tcp_connection(
    server: ServerPtr,
    stream: Stream,
    addr: SocketAddr,
    secure: bool,
    control_permissions: Option<ControlPermissions>,
) -> ResultType<()> {
    let mut stream = stream;
    let id = server.write().unwrap().get_new_id();

    if secure {
        let (sk, pk) = Config::get_key_pair();
        if pk.len() == sign::PUBLICKEYBYTES && sk.len() == sign::SECRETKEYBYTES {
            let mut sk_ = [0u8; sign::SECRETKEYBYTES];
            sk_[..].copy_from_slice(&sk);
            let sk = sign::SecretKey(sk_);
            let mut msg_out = Message::new();
            let (our_pk_b, our_sk_b) = box_::gen_keypair();
            msg_out.set_signed_id(SignedId {
                id: sign::sign(
                    &IdPk {
                        id: Config::get_id(),
                        pk: Bytes::from(our_pk_b.0.to_vec()),
                        ..Default::default()
                    }
                    .write_to_bytes()
                    .unwrap_or_default(),
                    &sk,
                )
                .into(),
                ..Default::default()
            });
            timeout(CONNECT_TIMEOUT, stream.send(&msg_out)).await??;
            match timeout(CONNECT_TIMEOUT, stream.next()).await? {
                Some(res) => {
                    let bytes = res?;
                    if let Ok(msg_in) = Message::parse_from_bytes(&bytes) {
                        if let Some(message::Union::PublicKey(pk)) = msg_in.union {
                            if pk.asymmetric_value.len() == box_::PUBLICKEYBYTES {
                                stream.set_key(hbb_common::tcp::Encrypt::decode(
                                    &pk.symmetric_value,
                                    &pk.asymmetric_value,
                                    &our_sk_b,
                                )?);
                            } else if pk.asymmetric_value.is_empty() {
                                Config::set_key_confirmed(false);
                                log::info!("Force to update pk");
                            } else {
                                bail!("Handshake failed: invalid public sign key length from peer");
                            }
                        }
                    }
                }
                None => {
                    bail!("Failed to receive public key");
                }
            }
        }
    }

    Connection::start(addr, stream, id, Arc::downgrade(&server), control_permissions).await;
    Ok(())
}

pub async fn create_relay_connection(
    server: ServerPtr,
    relay_server: String,
    uuid: String,
    peer_addr: SocketAddr,
    secure: bool,
    ipv4: bool,
) {
    if let Err(err) =
        create_relay_connection_(server, relay_server, uuid.clone(), peer_addr, secure, ipv4).await
    {
        log::error!(
            "Failed to create relay connection for {} with uuid {}: {}",
            peer_addr,
            uuid,
            err
        );
    }
}

async fn create_relay_connection_(
    server: ServerPtr,
    relay_server: String,
    uuid: String,
    peer_addr: SocketAddr,
    secure: bool,
    ipv4: bool,
) -> ResultType<()> {
    let mut stream = socket_client::connect_tcp(
        socket_client::ipv4_to_ipv6(crate::check_port(&relay_server, RELAY_PORT), ipv4),
        CONNECT_TIMEOUT,
    )
    .await?;
    let mut msg_out = RendezvousMessage::new();
    let licence_key = crate::get_key(true).await;
    msg_out.set_request_relay(RequestRelay {
        licence_key,
        uuid,
        ..Default::default()
    });
    stream.send(&msg_out).await?;
    create_tcp_connection(server, stream, peer_addr, secure, None).await?;
    Ok(())
}

// ============================================================
// ControlPermissions (stub for OHOS)
// ============================================================

#[derive(Clone, Debug, Default)]
pub struct ControlPermissions;
