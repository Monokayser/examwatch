//! Media Foundation H.264 encoder with two back ends:
//!
//! * **Hardware**: the async hardware MFT that belongs to the display adapter
//!   (NVENC on every lab PC that has one). Input is the NV12 D3D11 texture the
//!   scaler produced, so frames never leave the GPU.
//! * **Software**: Windows' built-in `CLSID_MSH264EncoderMFT`, fed NV12 bytes
//!   from system memory, one worker thread, speed preset. Used on the GT 730 labs.
//!
//! Both output Annex-B access units with SPS/PPS in front of every IDR.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{Interface, GUID, VARIANT};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};

use crate::nal;

#[derive(Clone, Copy, Debug)]
pub struct EncoderParams {
    pub width: u32,
    pub height: u32,
    /// Nominal frame rate (used for rate control; actual frames are variable-rate).
    pub fps: u32,
    pub avg_kbps: u32,
    pub max_kbps: u32,
    /// Upper QP bound so text stays legible (§2.9). Best effort: ignored if unsupported.
    pub max_qp: u32,
    /// Seconds between forced keyframes.
    pub gop_secs: u32,
}

pub struct Encoded {
    pub data: Vec<u8>,
    pub key: bool,
}

pub enum Input<'a> {
    Texture(&'a ID3D11Texture2D),
    Nv12(&'a [u8]),
}

pub struct H264Encoder {
    mft: IMFTransform,
    codec: Option<ICodecAPI>,
    events: Option<IMFMediaEventGenerator>,
    hardware: bool,
    p: EncoderParams,
    need_input: u32,
    provides_samples: bool,
    out_size: u32,
    seq_header: Vec<u8>,
    last_key: Option<Instant>,
    force_key: bool,
    pub name: String,
}

// The MFT is only touched from the capture thread that created it.
unsafe impl Send for H264Encoder {}

pub fn startup() -> Result<()> {
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED).ok().ok();
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
    }
    Ok(())
}

fn video_type(subtype: &GUID, p: &EncoderParams) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((p.width as u64) << 32) | p.height as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((p.fps.max(1) as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        if *subtype == MFVideoFormat_H264 {
            t.SetUINT32(&MF_MT_AVG_BITRATE, p.avg_kbps * 1000)?;
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
        }
        Ok(t)
    }
}

fn set_u32(codec: &ICodecAPI, api: &GUID, v: u32, what: &str) {
    let var = VARIANT::from(v);
    if let Err(e) = unsafe { codec.SetValue(api, &var) } {
        tracing::debug!("codec property {what} not applied: {e}");
    }
}

fn set_bool(codec: &ICodecAPI, api: &GUID, v: bool, what: &str) {
    let var = VARIANT::from(v);
    if let Err(e) = unsafe { codec.SetValue(api, &var) } {
        tracing::debug!("codec property {what} not applied: {e}");
    }
}

fn configure_codec(codec: &ICodecAPI, p: &EncoderParams, software: bool) {
    set_u32(codec, &CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32, "rate control");
    set_u32(codec, &CODECAPI_AVEncCommonMeanBitRate, p.avg_kbps * 1000, "mean bitrate");
    set_u32(codec, &CODECAPI_AVEncCommonMaxBitRate, p.max_kbps * 1000, "max bitrate");
    set_u32(codec, &CODECAPI_AVEncVideoMaxQP, p.max_qp, "max qp");
    set_u32(codec, &CODECAPI_AVEncMPVGOPSize, p.fps.max(1) * p.gop_secs.max(1), "gop");
    set_u32(codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0, "b-frames");
    set_bool(codec, &CODECAPI_AVLowLatencyMode, true, "low latency");
    // 0 = fastest. Screen content still looks sharp at our low frame rates.
    set_u32(codec, &CODECAPI_AVEncCommonQualityVsSpeed, if software { 0 } else { 50 }, "quality/speed");
    if software {
        set_u32(codec, &CODECAPI_AVEncNumWorkerThreads, 1, "worker threads");
    }
}

impl H264Encoder {
    /// Hardware MFT on the adapter with `luid`, initialised with that adapter's D3D device manager.
    pub fn hardware(p: EncoderParams, luid: LUID, devmgr: &IMFDXGIDeviceManager) -> Result<Self> {
        unsafe {
            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
            let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.context("attributes")?;
            let luid64 = ((luid.HighPart as u64) << 32) | luid.LowPart as u64;
            attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, std::slice::from_raw_parts(&luid64 as *const u64 as *const u8, 8))?;

            let mut acts: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut n = 0u32;
            MFTEnum2(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &attrs,
                &mut acts,
                &mut n,
            )?;
            if n == 0 || acts.is_null() {
                bail!("no hardware H.264 encoder on this adapter");
            }
            let list = std::slice::from_raw_parts_mut(acts, n as usize);
            let act = list[0].clone().context("null activate")?;
            let name = {
                let mut ptr = windows::core::PWSTR::null();
                let mut len = 0u32;
                if act.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len).is_ok() {
                    let s = ptr.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(ptr.0 as _));
                    s
                } else {
                    "hardware encoder".into()
                }
            };
            for a in list.iter_mut() {
                drop(a.take());
            }
            CoTaskMemFree(Some(acts as _));

            let mft: IMFTransform = act.ActivateObject()?;
            let a = mft.GetAttributes()?;
            a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, devmgr.as_raw() as usize)?;
            let codec = mft.cast::<ICodecAPI>().ok();
            if let Some(c) = &codec {
                configure_codec(c, &p, false);
            }
            mft.SetOutputType(0, &video_type(&MFVideoFormat_H264, &p)?, 0)?;
            mft.SetInputType(0, &video_type(&MFVideoFormat_NV12, &p)?, 0)?;
            let events = mft.cast::<IMFMediaEventGenerator>()?;
            let mut enc = Self::finish(mft, codec, Some(events), true, p, name)?;
            enc.start()?;
            Ok(enc)
        }
    }

    /// Windows' built-in software encoder.
    pub fn software(p: EncoderParams) -> Result<Self> {
        unsafe {
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                .context("Microsoft H.264 encoder is not installed (N edition without Media Feature Pack?)")?;
            let codec = mft.cast::<ICodecAPI>().ok();
            if let Some(c) = &codec {
                configure_codec(c, &p, true);
            }
            mft.SetOutputType(0, &video_type(&MFVideoFormat_H264, &p)?, 0)?;
            mft.SetInputType(0, &video_type(&MFVideoFormat_NV12, &p)?, 0)?;
            let mut enc = Self::finish(mft, codec, None, false, p, "Microsoft H.264 (software)".into())?;
            enc.start()?;
            Ok(enc)
        }
    }

    unsafe fn finish(mft: IMFTransform, codec: Option<ICodecAPI>, events: Option<IMFMediaEventGenerator>, hardware: bool, p: EncoderParams, name: String) -> Result<Self> {
        let info = mft.GetOutputStreamInfo(0)?;
        let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
        let mut seq = Vec::new();
        if let Ok(t) = mft.GetOutputCurrentType(0) {
            if let Ok(n) = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                seq.resize(n as usize, 0);
                if t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut seq, None).is_err() {
                    seq.clear();
                }
            }
        }
        Ok(Self {
            mft,
            codec,
            events,
            hardware,
            p,
            need_input: 0,
            provides_samples: provides,
            out_size: info.cbSize.max(p.width * p.height),
            seq_header: nal::to_annexb(&seq),
            last_key: None,
            force_key: true,
            name,
        })
    }

    unsafe fn start(&mut self) -> Result<()> {
        self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        Ok(())
    }

    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    pub fn params(&self) -> EncoderParams {
        self.p
    }

    /// Next frame will be an IDR (new viewer, reconnect, segment start).
    pub fn request_keyframe(&mut self) {
        self.force_key = true;
    }

    /// Encodes one frame. `ts_us` is the capture time. Returns 0..n access units
    /// (normally exactly one, since B-frames and lookahead are off).
    pub fn encode(&mut self, input: Input<'_>, ts_us: u64) -> Result<Vec<Encoded>> {
        let gop = Duration::from_secs(self.p.gop_secs.max(1) as u64);
        if self.last_key.map_or(true, |t| t.elapsed() >= gop) {
            self.force_key = true;
        }
        if self.force_key {
            if let Some(c) = &self.codec {
                set_u32(c, &CODECAPI_AVEncVideoForceKeyFrame, 1, "force keyframe");
            }
        }
        let sample = unsafe { self.make_sample(input, ts_us)? };
        let mut out = Vec::new();
        unsafe {
            if self.hardware {
                self.pump_events(&mut out, Duration::from_millis(200), true)?;
                self.mft.ProcessInput(0, &sample, 0)?;
                self.need_input = self.need_input.saturating_sub(1);
                // NVENC turns a frame around in a few ms; wait for it so the
                // frame goes out now rather than one capture interval later.
                self.pump_events(&mut out, Duration::from_millis(100), false)?;
            } else {
                self.mft.ProcessInput(0, &sample, 0)?;
                self.drain(&mut out)?;
            }
        }
        if out.iter().any(|e| e.key) {
            self.force_key = false;
            self.last_key = Some(Instant::now());
        }
        Ok(out)
    }

    unsafe fn make_sample(&self, input: Input<'_>, ts_us: u64) -> Result<IMFSample> {
        let sample = MFCreateSample()?;
        let buf = match input {
            Input::Texture(tex) => MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, tex, 0, false)?,
            Input::Nv12(bytes) => {
                let expect = (self.p.width * self.p.height * 3 / 2) as usize;
                if bytes.len() != expect {
                    bail!("NV12 frame is {} bytes, expected {expect}", bytes.len());
                }
                let b = MFCreateMemoryBuffer(bytes.len() as u32)?;
                let mut ptr = std::ptr::null_mut();
                b.Lock(&mut ptr, None, None)?;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
                b.Unlock()?;
                b.SetCurrentLength(bytes.len() as u32)?;
                b
            }
        };
        sample.AddBuffer(&buf)?;
        sample.SetSampleTime((ts_us * 10) as i64)?;
        sample.SetSampleDuration(10_000_000 / self.p.fps.max(1) as i64)?;
        Ok(sample)
    }

    /// Async (hardware) MFT event loop. With `until_need_input` it returns once
    /// the MFT accepts input; otherwise once it has produced one output (or timeout).
    unsafe fn pump_events(&mut self, out: &mut Vec<Encoded>, budget: Duration, until_need_input: bool) -> Result<()> {
        let ev = self.events.clone().context("not an async MFT")?;
        let start = Instant::now();
        let produced_before = out.len();
        loop {
            if until_need_input && self.need_input > 0 {
                return Ok(());
            }
            if !until_need_input && out.len() > produced_before {
                return Ok(());
            }
            match ev.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(e) => {
                    let t = e.GetType()?;
                    if t == METransformNeedInput.0 as u32 {
                        self.need_input += 1;
                    } else if t == METransformHaveOutput.0 as u32 {
                        self.drain(out)?;
                    } else if t == METransformDrainComplete.0 as u32 {
                        // nothing
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if start.elapsed() >= budget {
                        if until_need_input {
                            bail!("hardware encoder did not request input within {budget:?}");
                        }
                        return Ok(()); // output will be collected with the next frame
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    unsafe fn drain(&mut self, out: &mut Vec<Encoded>) -> Result<()> {
        loop {
            let own_sample = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                Some(s)
            };
            let mut db = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(own_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut db, &mut status);
            let sample = ManuallyDrop::take(&mut db[0].pSample);
            drop(ManuallyDrop::take(&mut db[0].pEvents));
            match r {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // Re-negotiate the output type (some MFTs do this on the first frame).
                    let t = self.mft.GetOutputAvailableType(0, 0)?;
                    self.mft.SetOutputType(0, &t, 0)?;
                    continue;
                }
                Err(e) => return Err(anyhow!("ProcessOutput failed: {e}")),
            }
            let Some(sample) = sample else { return Ok(()) };
            let buf = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut ptr, None, Some(&mut len))?;
            let mut data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buf.Unlock()?;
            let key = nal::is_idr(&data);
            if key && !nal::has_sps(&data) && !self.seq_header.is_empty() {
                let mut v = self.seq_header.clone();
                v.extend_from_slice(&data);
                data = v;
            }
            if !data.is_empty() {
                out.push(Encoded { data, key });
            }
            if !self.hardware {
                continue; // a sync MFT may hold more than one output
            }
            return Ok(());
        }
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Some(ev) = &self.events {
                if let Ok(s) = ev.cast::<IMFShutdown>() {
                    let _ = s.Shutdown();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::Synth;

    /// CI runners (Windows Server) may lack Media Foundation's H.264 encoder.
    /// Set EW_REQUIRE_MF=1 on real PCs so a missing encoder fails the test.
    fn sw(p: EncoderParams) -> Option<H264Encoder> {
        match H264Encoder::software(p) {
            Ok(e) => Some(e),
            Err(e) if std::env::var_os("EW_REQUIRE_MF").is_none() => {
                eprintln!("skipping: software H.264 encoder unavailable: {e:#}");
                None
            }
            Err(e) => panic!("{e:#}"),
        }
    }

    fn params(w: u32, h: u32) -> EncoderParams {
        EncoderParams { width: w, height: h, fps: 2, avg_kbps: 250, max_kbps: 600, max_qp: 30, gop_secs: 10 }
    }

    #[test]
    fn software_encoder_produces_idr_with_sps_then_p_frames() {
        startup().unwrap();
        let Some(mut enc) = sw(params(1280, 720)) else { return };
        let mut s = Synth::new(1280, 720, 1);
        let mut frames = Vec::new();
        for i in 0..6u64 {
            let ts = 1_700_000_000_000_000 + i * 500_000;
            frames.extend(enc.encode(Input::Nv12(s.frame(ts / 1000, 1)), ts).unwrap());
        }
        assert!(!frames.is_empty(), "encoder produced nothing");
        assert!(frames[0].key, "first frame must be IDR");
        assert!(nal::has_sps(&frames[0].data), "IDR must carry SPS/PPS");
        assert!(frames.iter().skip(1).any(|f| !f.key), "later frames should be P-frames");
        let idr = frames[0].data.len();
        let p = frames[1..].iter().filter(|f| !f.key).map(|f| f.data.len()).max().unwrap();
        assert!(p < idr, "P-frames ({p}) should be smaller than the IDR ({idr})");
    }

    #[test]
    fn software_encoder_honours_keyframe_request() {
        startup().unwrap();
        let Some(mut enc) = sw(params(480, 270)) else { return };
        let mut s = Synth::new(480, 270, 2);
        let mut keys = Vec::new();
        for i in 0..5u64 {
            if i == 3 {
                enc.request_keyframe();
            }
            let ts = i * 1_000_000;
            for f in enc.encode(Input::Nv12(s.frame(ts / 1000, 2)), ts).unwrap() {
                keys.push(f.key);
            }
        }
        assert_eq!(keys.iter().filter(|k| **k).count(), 2, "initial + requested keyframe: {keys:?}");
    }
}
