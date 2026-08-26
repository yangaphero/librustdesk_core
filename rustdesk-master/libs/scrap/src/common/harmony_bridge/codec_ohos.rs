use std::{ffi::c_void, sync::{Arc, Mutex}};

use hbb_common::{
    anyhow::{anyhow, Context},
    bytes::Bytes,
    config::{Config, PeerConfig},
    log,
    message_proto::{
        supported_decoding::PreferCodec, video_frame, Chroma, EncodedVideoFrame,
        EncodedVideoFrames, SupportedDecoding, SupportedEncoding, VideoFrame,
    },
    ResultType,
};

use super::{
    vpxcodec::{self, VpxDecoder, VpxDecoderConfig, VpxEncoder, VpxEncoderConfig, VpxVideoCodecId},
    GoogleImage,
};
use crate::{CodecFormat, EncodeInput, EncodeYuvFormat, ImageRgb, ImageTexture};

lazy_static::lazy_static! {
    static ref PEER_DECODINGS: Arc<Mutex<std::collections::HashMap<i32, SupportedDecoding>>> = Default::default();
    static ref ENCODE_CODEC_FORMAT: Arc<Mutex<CodecFormat>> = Arc::new(Mutex::new(CodecFormat::VP9));
    static ref USABLE_ENCODING: Arc<Mutex<Option<SupportedEncoding>>> = Arc::new(Mutex::new(None));
}

pub const ENCODE_NEED_SWITCH: &'static str = "ENCODE_NEED_SWITCH";

#[derive(Debug, Clone)]
pub enum EncoderCfg {
    VPX(VpxEncoderConfig),
}

pub trait EncoderApi {
    fn new(cfg: EncoderCfg, i444: bool) -> ResultType<Self>
    where
        Self: Sized;
    fn encode_to_message(&mut self, frame: EncodeInput<'_>, ms: i64) -> ResultType<VideoFrame>;
    fn yuvfmt(&self) -> EncodeYuvFormat;
    fn set_quality(&mut self, ratio: f32) -> ResultType<()>;
    fn bitrate(&self) -> u32;
    fn support_changing_quality(&self) -> bool;
    fn latency_free(&self) -> bool;
    fn is_hardware(&self) -> bool;
    fn disable(&self);
}

pub struct Encoder {
    vpx: VpxEncoder,
}

pub struct Decoder {
    format: CodecFormat,
    valid: bool,
    vp8: Option<VpxDecoder>,
    vp9: Option<VpxDecoder>,
    native: Option<OhosVideoDecoder>,
}

const OHOS_CODEC_AV1: i32 = 3;
const OHOS_CODEC_H264: i32 = 4;
const OHOS_CODEC_H265: i32 = 5;
const OHOS_PIXEL_FORMAT_I420: i32 = 1;
const OHOS_PIXEL_FORMAT_NV12: i32 = 2;

unsafe extern "C" {
    fn rustdesk_ohos_video_decoder_is_supported(codec: i32) -> bool;
    fn rustdesk_ohos_video_decoder_create(codec: i32) -> *mut c_void;
    fn rustdesk_ohos_video_decoder_destroy(handle: *mut c_void);
    fn rustdesk_ohos_video_decoder_submit(
        handle: *mut c_void,
        data: *const u8,
        size: usize,
        key: bool,
        presentation_time_us: i64,
    ) -> i32;
    fn rustdesk_ohos_video_decoder_frame_info(
        handle: *mut c_void,
        width: *mut i32,
        height: *mut i32,
        stride: *mut i32,
        slice_height: *mut i32,
        pixel_format: *mut i32,
    ) -> i64;
    fn rustdesk_ohos_video_decoder_copy_frame(
        handle: *mut c_void,
        output: *mut u8,
        capacity: usize,
    ) -> i64;
    fn rustdesk_ohos_video_decoder_lock_output(handle: *mut c_void, out_len: *mut i64) -> *const u8;
    fn rustdesk_ohos_video_decoder_unlock_output(handle: *mut c_void);
}

struct OhosVideoDecoder {
    handle: *mut c_void,
    frame: Vec<u8>,
}

unsafe impl Send for OhosVideoDecoder {}

impl OhosVideoDecoder {
    fn codec_id(format: CodecFormat) -> Option<i32> {
        match format {
            CodecFormat::AV1 => Some(OHOS_CODEC_AV1),
            CodecFormat::H264 => Some(OHOS_CODEC_H264),
            CodecFormat::H265 => Some(OHOS_CODEC_H265),
            _ => None,
        }
    }

    fn is_supported(format: CodecFormat) -> bool {
        Self::codec_id(format)
            .map(|codec| unsafe { rustdesk_ohos_video_decoder_is_supported(codec) })
            .unwrap_or(false)
    }

    fn new(format: CodecFormat) -> Option<Self> {
        let codec = Self::codec_id(format)?;
        let handle = unsafe { rustdesk_ohos_video_decoder_create(codec) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                frame: Vec::new(),
            })
        }
    }

    fn decode(&mut self, frames: &EncodedVideoFrames, rgb: &mut ImageRgb) -> ResultType<bool> {
        let mut produced = false;
        // Hardware decoders pipeline: an accepted input may not yield its
        // output within this call. Treat successful submission as progress
        // (Ok(true)) so the caller's fail-counter only trips on real errors;
        // frames surface through subsequent polls.
        let mut accepted = false;
        for frame in &frames.frames {
            let status = unsafe {
                rustdesk_ohos_video_decoder_submit(
                    self.handle,
                    frame.data.as_ptr(),
                    frame.data.len(),
                    frame.key,
                    frame.pts,
                )
            };
            if status < 0 {
                return Err(anyhow!("OHOS native video decoder submit failed: {status}"));
            }
            if status == 0 {
                continue;
            }
            accepted = true;

            let mut width = 0;
            let mut height = 0;
            let mut stride = 0;
            let mut slice_height = 0;
            let mut pixel_format = 0;
            let frame_size = unsafe {
                rustdesk_ohos_video_decoder_frame_info(
                    self.handle,
                    &mut width,
                    &mut height,
                    &mut stride,
                    &mut slice_height,
                    &mut pixel_format,
                )
            };
            if frame_size <= 0 || width <= 0 || height <= 0 || stride <= 0 {
                continue;
            }
            // Zero-copy path: convert directly from the codec output buffer.
            let mut out_len: i64 = 0;
            let src = unsafe { rustdesk_ohos_video_decoder_lock_output(self.handle, &mut out_len) };
            if src.is_null() || out_len < frame_size {
                if !src.is_null() {
                    unsafe { rustdesk_ohos_video_decoder_unlock_output(self.handle) };
                }
                continue;
            }
            let src_slice = unsafe { std::slice::from_raw_parts(src, out_len as usize) };
            let convert_result = Self::convert_frame(
                src_slice,
                width as usize,
                height as usize,
                stride as usize,
                std::cmp::max(height, slice_height) as usize,
                pixel_format,
                rgb,
            );
            unsafe { rustdesk_ohos_video_decoder_unlock_output(self.handle) };
            convert_result?;
            produced = true;
        }
        Ok(produced || accepted)
    }

    fn convert_frame(
        frame: &[u8],
        width: usize,
        height: usize,
        stride: usize,
        slice_height: usize,
        pixel_format: i32,
        rgb: &mut ImageRgb,
    ) -> ResultType<()> {
        let bytes_per_row = (width * 4 + rgb.align() - 1) & !(rgb.align() - 1);
        rgb.w = width;
        rgb.h = height;
        rgb.raw.resize(height * bytes_per_row, 0);
        let y_size = stride * slice_height;
        if frame.len() < y_size {
            return Err(anyhow!("OHOS decoder frame is shorter than the Y plane"));
        }
        unsafe {
            match pixel_format {
                OHOS_PIXEL_FORMAT_I420 => {
                    let uv_stride = (stride + 1) / 2;
                    let uv_height = (slice_height + 1) / 2;
                    let uv_size = uv_stride * uv_height;
                    if frame.len() < y_size + uv_size * 2 {
                        return Err(anyhow!("OHOS I420 decoder frame is incomplete"));
                    }
                    let y = frame.as_ptr();
                    let u = frame[y_size..].as_ptr();
                    let v = frame[y_size + uv_size..].as_ptr();
                    match rgb.fmt() {
                        crate::ImageFormat::ARGB => super::I420ToARGB(
                            y,
                            stride as i32,
                            u,
                            uv_stride as i32,
                            v,
                            uv_stride as i32,
                            rgb.raw.as_mut_ptr(),
                            bytes_per_row as i32,
                            width as i32,
                            height as i32,
                        ),
                        crate::ImageFormat::ABGR => super::I420ToABGR(
                            y,
                            stride as i32,
                            u,
                            uv_stride as i32,
                            v,
                            uv_stride as i32,
                            rgb.raw.as_mut_ptr(),
                            bytes_per_row as i32,
                            width as i32,
                            height as i32,
                        ),
                        _ => return Err(anyhow!("unsupported OHOS decoder RGB format")),
                    };
                }
                OHOS_PIXEL_FORMAT_NV12 => {
                    if frame.len() < y_size + stride * ((slice_height + 1) / 2) {
                        return Err(anyhow!("OHOS NV12 decoder frame is incomplete"));
                    }
                    let y = frame.as_ptr();
                    let uv = frame[y_size..].as_ptr();
                    match rgb.fmt() {
                        crate::ImageFormat::ARGB => super::NV12ToARGB(
                            y,
                            stride as i32,
                            uv,
                            stride as i32,
                            rgb.raw.as_mut_ptr(),
                            bytes_per_row as i32,
                            width as i32,
                            height as i32,
                        ),
                        crate::ImageFormat::ABGR => super::NV12ToABGR(
                            y,
                            stride as i32,
                            uv,
                            stride as i32,
                            rgb.raw.as_mut_ptr(),
                            bytes_per_row as i32,
                            width as i32,
                            height as i32,
                        ),
                        _ => return Err(anyhow!("unsupported OHOS decoder RGB format")),
                    };
                }
                _ => return Err(anyhow!("unsupported OHOS decoder pixel format: {pixel_format}")),
            }
        }
        Ok(())
    }
}

impl Drop for OhosVideoDecoder {
    fn drop(&mut self) {
        unsafe { rustdesk_ohos_video_decoder_destroy(self.handle) };
    }
}

#[derive(Debug, Clone)]
pub enum EncodingUpdate {
    Update(i32, SupportedDecoding),
    Remove(i32),
    NewOnlyVP9(i32),
    Check,
}

fn create_frame(frame: &vpxcodec::EncodeFrame) -> EncodedVideoFrame {
    EncodedVideoFrame {
        data: Bytes::from(frame.data.to_vec()),
        key: frame.key,
        pts: frame.pts,
        ..Default::default()
    }
}

impl Encoder {
    pub fn new(config: EncoderCfg, i444: bool) -> ResultType<Encoder> {
        log::info!("OHOS new encoder: {config:?}, i444: {i444}");
        let vpx = VpxEncoder::new(config, i444)?;
        *ENCODE_CODEC_FORMAT.lock().unwrap() = match vpx.codec_id() {
            VpxVideoCodecId::VP8 => CodecFormat::VP8,
            VpxVideoCodecId::VP9 => CodecFormat::VP9,
        };
        Ok(Encoder { vpx })
    }

    pub fn yuvfmt(&self) -> crate::EncodeYuvFormat {
        use crate::codec::EncoderApi;
        self.vpx.yuvfmt()
    }

    pub fn latency_free(&self) -> bool {
        use crate::codec::EncoderApi;
        self.vpx.latency_free()
    }

    pub fn bitrate(&self) -> u32 {
        use crate::codec::EncoderApi;
        self.vpx.bitrate()
    }

    pub fn support_changing_quality(&self) -> bool {
        use crate::codec::EncoderApi;
        self.vpx.support_changing_quality()
    }

    pub fn is_hardware(&self) -> bool {
        false
    }

    pub fn disable(&self) {}

    #[inline]
    pub fn negotiated_codec() -> CodecFormat {
        ENCODE_CODEC_FORMAT.lock().unwrap().clone()
    }

    pub fn set_fallback(config: &EncoderCfg) {
        let format = match config {
            EncoderCfg::VPX(vpx) => match vpx.codec {
                VpxVideoCodecId::VP8 => CodecFormat::VP8,
                VpxVideoCodecId::VP9 => CodecFormat::VP9,
            },
        };
        *ENCODE_CODEC_FORMAT.lock().unwrap() = format;
    }

    pub fn use_i444(_config: &EncoderCfg) -> bool {
        false
    }

    pub fn usable_encoding() -> SupportedEncoding {
        SupportedEncoding {
            vp8: true,
            av1: false,
            h264: false,
            h265: false,
            ..Default::default()
        }
    }

    pub fn update(update: EncodingUpdate) {
        log::info!("OHOS update:{:?}", update);
        let mut decodings = PEER_DECODINGS.lock().unwrap();
        match update {
            EncodingUpdate::Update(id, decoding) => {
                decodings.insert(id, decoding);
            }
            EncodingUpdate::Remove(id) => {
                decodings.remove(&id);
            }
            EncodingUpdate::NewOnlyVP9(id) => {
                decodings.insert(
                    id,
                    SupportedDecoding {
                        ability_vp9: 1,
                        prefer: PreferCodec::VP9.into(),
                        ..Default::default()
                    },
                );
            }
            EncodingUpdate::Check => {}
        }
        let decodings = decodings.clone();
        let mut encoding = Self::supported_encoding();
        let decodable_vp8 = decodings.iter().all(|d| d.1.ability_vp8 > 0);
        if !decodable_vp8 {
            encoding.vp8 = false;
        }
        *USABLE_ENCODING.lock().unwrap() = Some(encoding);
    }

    pub fn supported_encoding() -> SupportedEncoding {
        SupportedEncoding {
            vp8: true,
            av1: false,
            h264: false,
            h265: false,
            ..Default::default()
        }
    }

    pub fn set_bitrate(&mut self, bitrate: u32) {
        let _ = self.vpx.set_bitrate(bitrate);
    }

    pub fn encode_to_message(&mut self, frame: EncodeInput<'_>, ms: i64) -> ResultType<VideoFrame> {
        let mut frames = Vec::new();
        for ref f in self
            .vpx
            .encode(ms, frame.yuv()?, crate::STRIDE_ALIGN)
            .with_context(|| "Failed to encode")?
        {
            frames.push(create_frame(f));
        }
        for ref f in self.vpx.flush().with_context(|| "Failed to flush")? {
            frames.push(create_frame(f));
        }

        if !frames.is_empty() {
            Ok(VpxEncoder::create_video_frame(self.vpx.codec_id(), frames))
        } else {
            Err(anyhow!("no valid frame"))
        }
    }
}

impl Decoder {
    pub fn supported_decodings(
        id: Option<&str>,
        _use_texture_render: bool,
        _luid: Option<i64>,
        mark_unsupported: &[CodecFormat],
    ) -> SupportedDecoding {
        let configured_codec = id
            .filter(|peer_id| !peer_id.is_empty())
            .and_then(|peer_id| {
                PeerConfig::load(peer_id)
                    .options
                    .get("codec-preference")
                    .cloned()
            })
            .filter(|codec| !codec.is_empty())
            .unwrap_or_else(|| Config::get_option("codec-preference"));
        let preference = Some(configured_codec)
            .map(|codec| match codec.as_str() {
                "vp8" => PreferCodec::VP8,
                "vp9" => PreferCodec::VP9,
                "av1" => PreferCodec::AV1,
                "h264" => PreferCodec::H264,
                "h265" => PreferCodec::H265,
                _ => PreferCodec::Auto,
            })
            .unwrap_or(PreferCodec::Auto);
        let mut decoding = SupportedDecoding {
            ability_vp8: 1,
            ability_vp9: 1,
            ability_av1: i32::from(OhosVideoDecoder::is_supported(CodecFormat::AV1)),
            ability_h264: i32::from(OhosVideoDecoder::is_supported(CodecFormat::H264)),
            ability_h265: i32::from(OhosVideoDecoder::is_supported(CodecFormat::H265)),
            prefer: preference.into(),
            ..Default::default()
        };
        for unsupported in mark_unsupported {
            match unsupported {
                CodecFormat::VP8 => decoding.ability_vp8 = 0,
                CodecFormat::VP9 => decoding.ability_vp9 = 0,
                CodecFormat::AV1 => decoding.ability_av1 = 0,
                CodecFormat::H264 => decoding.ability_h264 = 0,
                CodecFormat::H265 => decoding.ability_h265 = 0,
                _ => {}
            }
        }
        decoding
    }

    pub fn new(format: CodecFormat, _luid: Option<i64>) -> Decoder {
        log::info!("try create new decoder on ohos, format: {format:?}");
        let vp8 = VpxDecoder::new(VpxDecoderConfig {
            codec: VpxVideoCodecId::VP8,
        })
        .map_err(|err| {
            log::error!("failed to create OHOS VP8 decoder: {err}");
            err
        })
        .ok();
        let vp9 = VpxDecoder::new(VpxDecoderConfig {
            codec: VpxVideoCodecId::VP9,
        })
        .map_err(|err| {
            log::error!("failed to create OHOS VP9 decoder: {err}");
            err
        })
        .ok();
        let valid = match format {
            CodecFormat::VP8 => vp8.is_some(),
            CodecFormat::VP9 => vp9.is_some(),
            CodecFormat::AV1 | CodecFormat::H264 | CodecFormat::H265 => {
                OhosVideoDecoder::is_supported(format)
            }
            _ => false,
        };
        let native = if matches!(format, CodecFormat::AV1 | CodecFormat::H264 | CodecFormat::H265) {
            OhosVideoDecoder::new(format)
        } else {
            None
        };
        Decoder {
            format,
            valid: valid && (!matches!(format, CodecFormat::AV1 | CodecFormat::H264 | CodecFormat::H265) || native.is_some()),
            vp8,
            vp9,
            native,
        }
    }

    pub fn format(&self) -> CodecFormat {
        self.format
    }

    pub fn valid(&self) -> bool {
        self.valid
    }

    pub fn handle_video_frame(
        &mut self,
        frame: &video_frame::Union,
        rgb: &mut ImageRgb,
        _texture: &mut ImageTexture,
        _pixelbuffer: &mut bool,
        chroma: &mut Option<Chroma>,
    ) -> ResultType<bool> {
        match frame {
            video_frame::Union::Vp8s(vp8s) => {
                if let Some(vp8) = &mut self.vp8 {
                    Self::handle_vpxs_video_frame(vp8, vp8s, rgb, chroma)
                } else {
                    Err(anyhow!("vp8 decoder not available on ohos"))
                }
            }
            video_frame::Union::Vp9s(vp9s) => {
                if let Some(vp9) = &mut self.vp9 {
                    Self::handle_vpxs_video_frame(vp9, vp9s, rgb, chroma)
                } else {
                    Err(anyhow!("vp9 decoder not available on ohos"))
                }
            }
            video_frame::Union::Av1s(frames)
            | video_frame::Union::H264s(frames)
            | video_frame::Union::H265s(frames) => {
                *chroma = Some(Chroma::I420);
                if let Some(native) = &mut self.native {
                    native.decode(frames, rgb)
                } else {
                    Err(anyhow!("native video decoder not available on ohos"))
                }
            }
            _ => Err(anyhow!("unsupported video frame type on ohos")),
        }
    }

    pub fn decode(&mut self, data: &[u8], rgb: &mut ImageRgb) -> ResultType<bool> {
        let mut frames = EncodedVideoFrames::new();
        frames.frames.push(EncodedVideoFrame {
            data: hbb_common::bytes::Bytes::from(data.to_vec()),
            ..Default::default()
        });
        let mut chroma = None;
        match self.format {
            CodecFormat::VP8 => {
                if let Some(vp8) = &mut self.vp8 {
                    Self::handle_vpxs_video_frame(vp8, &frames, rgb, &mut chroma)
                } else {
                    Err(anyhow!("vp8 decoder not available on ohos"))
                }
            }
            CodecFormat::VP9 => {
                if let Some(vp9) = &mut self.vp9 {
                    Self::handle_vpxs_video_frame(vp9, &frames, rgb, &mut chroma)
                } else {
                    Err(anyhow!("vp9 decoder not available on ohos"))
                }
            }
            CodecFormat::AV1 | CodecFormat::H264 | CodecFormat::H265 => {
                let mut frames = EncodedVideoFrames::new();
                frames.frames.push(EncodedVideoFrame {
                    data: hbb_common::bytes::Bytes::from(data.to_vec()),
                    ..Default::default()
                });
                if let Some(native) = &mut self.native {
                    native.decode(&frames, rgb)
                } else {
                    Err(anyhow!("native video decoder not available on ohos"))
                }
            }
            _ => Err(anyhow!("unsupported decoder format on ohos")),
        }
    }

    fn handle_vpxs_video_frame(
        decoder: &mut VpxDecoder,
        vpxs: &EncodedVideoFrames,
        rgb: &mut ImageRgb,
        chroma: &mut Option<Chroma>,
    ) -> ResultType<bool> {
        let mut last_frame = vpxcodec::Image::new();
        for vpx in vpxs.frames.iter() {
            for frame in decoder.decode(&vpx.data)? {
                drop(last_frame);
                last_frame = frame;
            }
        }
        for frame in decoder.flush()? {
            drop(last_frame);
            last_frame = frame;
        }
        if last_frame.is_null() {
            Ok(false)
        } else {
            *chroma = Some(last_frame.chroma());
            last_frame.to(rgb);
            Ok(true)
        }
    }
}

pub fn base_bitrate(width: u32, height: u32) -> u32 {
    const RESOLUTION_PRESETS: &[(u32, u32, u32)] = &[
        (640, 480, 400),
        (800, 600, 500),
        (1024, 768, 800),
        (1280, 720, 1000),
        (1366, 768, 1100),
        (1440, 900, 1300),
        (1600, 900, 1500),
        (1920, 1080, 2073),
        (2048, 1080, 2200),
        (2560, 1440, 3000),
        (3440, 1440, 4000),
        (3840, 2160, 5000),
        (7680, 4320, 12000),
    ];
    let pixels = width * height;

    let (preset_pixels, preset_bitrate) = RESOLUTION_PRESETS
        .iter()
        .map(|(w, h, bitrate)| (w * h, bitrate))
        .min_by_key(|(preset_pixels, _)| {
            if *preset_pixels >= pixels {
                preset_pixels - pixels
            } else {
                pixels - preset_pixels
            }
        })
        .unwrap_or(((1920 * 1080) as u32, &2073));

    (*preset_bitrate as f32 * (pixels as f32 / preset_pixels as f32)).round() as u32
}

pub fn codec_thread_num(limit: usize) -> usize {
    std::cmp::max(1, std::cmp::min(limit, 4))
}

pub fn enable_hwcodec_option() -> bool {
    false
}

pub fn enable_directx_capture() -> bool {
    false
}

pub fn test_av1() {}

pub const BR_BEST: f32 = 1.5;
pub const BR_BALANCED: f32 = 0.67;
pub const BR_SPEED: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quality {
    Best,
    Balanced,
    Low,
    Custom(f32),
}

impl Default for Quality {
    fn default() -> Self {
        Self::Balanced
    }
}

impl Quality {
    pub fn is_custom(&self) -> bool {
        match self {
            Quality::Custom(_) => true,
            _ => false,
        }
    }

    pub fn ratio(&self) -> f32 {
        match self {
            Quality::Best => BR_BEST,
            Quality::Balanced => BR_BALANCED,
            Quality::Low => BR_SPEED,
            Quality::Custom(v) => *v,
        }
    }
}
