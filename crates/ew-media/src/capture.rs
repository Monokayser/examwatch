//! DXGI Desktop Duplication of the primary monitor, plus GPU scaling to NV12.
//!
//! Zero-impact rules (design §2.8) enforced here:
//! * the duplicated frame is copied GPU→GPU and released immediately, so the
//!   compositor is never held up;
//! * no GDI, no hooks; the cursor is not drawn into frames;
//! * the D3D device runs at the lowest GPU thread priority;
//! * scaling uses the video processor (fixed-function on NVIDIA), at most a few
//!   small blits per second.

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::{BOOL, HMODULE, LUID, RECT};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};

pub struct Gpu {
    pub device: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    pub luid: LUID,
    pub adapter_name: String,
    pub output: IDXGIOutput1,
    pub screen: (u32, u32),
    pub monitors: u32,
}

/// Finds the adapter that drives the primary monitor and creates a low-priority device on it.
pub fn open_primary() -> Result<Gpu> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut monitors = 0u32;
        let mut found = None;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let mut j = 0;
            while let Ok(out) = adapter.EnumOutputs(j) {
                j += 1;
                let d = out.GetDesc()?;
                if !d.AttachedToDesktop.as_bool() {
                    continue;
                }
                monitors += 1;
                if d.DesktopCoordinates.left == 0 && d.DesktopCoordinates.top == 0 && found.is_none() {
                    let r = d.DesktopCoordinates;
                    found = Some((adapter.clone(), out, ((r.right - r.left) as u32, (r.bottom - r.top) as u32)));
                }
            }
        }
        let (adapter, out, screen) = found.context("no primary monitor attached")?;
        let ad = adapter.GetDesc1()?;
        let name = String::from_utf16_lossy(&ad.Description).trim_end_matches('\0').to_string();

        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )?;
        let device = device.context("no device")?;
        let ctx = ctx.context("no context")?;
        // Media Foundation uses the device from its own threads.
        if let Ok(mt) = device.cast::<ID3D10Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        // Lowest GPU scheduling priority: the student's apps always go first.
        if let Ok(dx) = device.cast::<IDXGIDevice>() {
            let _ = dx.SetGPUThreadPriority(-7);
        }
        Ok(Gpu { device, ctx, luid: ad.AdapterLuid, adapter_name: name, output: out.cast()?, screen, monitors })
    }
}

impl Gpu {
    pub fn device_manager(&self) -> Result<IMFDXGIDeviceManager> {
        unsafe {
            let mut token = 0u32;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
            let mgr = mgr.context("no device manager")?;
            mgr.ResetDevice(&self.device, token)?;
            Ok(mgr)
        }
    }
}

pub enum Grab {
    /// Screen content changed since the last grab; our copy is updated.
    Changed,
    /// Nothing new (static screen, or only the mouse moved).
    Unchanged,
    /// Desktop switched (UAC prompt, lock screen, Ctrl+Alt+Del) or mode change; re-open needed.
    Lost,
}

pub struct Duplicator {
    dup: IDXGIOutputDuplication,
    pub copy: ID3D11Texture2D,
    pub size: (u32, u32),
}

impl Duplicator {
    pub fn new(gpu: &Gpu) -> Result<Self> {
        unsafe {
            let dup = gpu.output.DuplicateOutput(&gpu.device).context("DuplicateOutput")?;
            let d = dup.GetDesc();
            let size = (d.ModeDesc.Width, d.ModeDesc.Height);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut copy = None;
            gpu.device.CreateTexture2D(&desc, None, Some(&mut copy))?;
            Ok(Self { dup, copy: copy.context("texture")?, size })
        }
    }

    /// Waits up to `timeout_ms` for a new desktop image.
    pub fn grab(&mut self, gpu: &Gpu, timeout_ms: u32) -> Result<Grab> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match self.dup.AcquireNextFrame(timeout_ms, &mut info, &mut res) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Grab::Unchanged),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST || e.code() == DXGI_ERROR_INVALID_CALL => return Ok(Grab::Lost),
                Err(e) => return Err(e.into()),
            }
            let changed = info.LastPresentTime != 0 && info.AccumulatedFrames > 0;
            if changed {
                if let Some(r) = &res {
                    let tex: ID3D11Texture2D = r.cast()?;
                    gpu.ctx.CopyResource(&self.copy, &tex);
                }
            }
            drop(res);
            // Release right away: holding the frame would make DWM wait for us.
            let _ = self.dup.ReleaseFrame();
            Ok(if changed { Grab::Changed } else { Grab::Unchanged })
        }
    }
}

/// BGRA (screen) → NV12 at a fixed output size, on the GPU video processor.
pub struct Scaler {
    // Kept alive for the lifetime of the views created from them.
    #[allow(dead_code)]
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    #[allow(dead_code)]
    enumr: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    out_view: ID3D11VideoProcessorOutputView,
    in_view: ID3D11VideoProcessorInputView,
    pub out: ID3D11Texture2D,
    staging: Option<ID3D11Texture2D>,
    pub size: (u32, u32),
}

impl Scaler {
    pub fn new(gpu: &Gpu, src: &ID3D11Texture2D, src_size: (u32, u32), out_size: (u32, u32)) -> Result<Self> {
        unsafe {
            let vdev: ID3D11VideoDevice = gpu.device.cast()?;
            let vctx: ID3D11VideoContext = gpu.ctx.cast()?;
            let cd = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                InputWidth: src_size.0,
                InputHeight: src_size.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                OutputWidth: out_size.0,
                OutputHeight: out_size.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
            };
            let enumr = vdev.CreateVideoProcessorEnumerator(&cd)?;
            let vp = vdev.CreateVideoProcessor(&enumr, 0)?;

            let desc = D3D11_TEXTURE2D_DESC {
                Width: out_size.0,
                Height: out_size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut out = None;
            gpu.device.CreateTexture2D(&desc, None, Some(&mut out))?;
            let out = out.context("nv12 texture")?;

            let ovd = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out_view = None;
            vdev.CreateVideoProcessorOutputView(&out, &enumr, &ovd, Some(&mut out_view))?;
            let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut in_view = None;
            vdev.CreateVideoProcessorInputView(src, &enumr, &ivd, Some(&mut in_view))?;

            // Full-range RGB in, BT.709 limited-range YCbCr out (what decoders assume).
            let in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            let out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
            vctx.VideoProcessorSetStreamColorSpace(&vp, 0, &in_cs);
            vctx.VideoProcessorSetOutputColorSpace(&vp, &out_cs);
            let full = RECT { left: 0, top: 0, right: src_size.0 as i32, bottom: src_size.1 as i32 };
            let dst = RECT { left: 0, top: 0, right: out_size.0 as i32, bottom: out_size.1 as i32 };
            vctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&full));
            vctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&dst));
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);

            Ok(Self {
                vdev,
                vctx,
                enumr,
                vp,
                out_view: out_view.context("output view")?,
                in_view: in_view.context("input view")?,
                out,
                staging: None,
                size: out_size,
            })
        }
    }

    /// Scales the current screen copy into `self.out` (NV12).
    pub fn run(&self) -> Result<()> {
        unsafe {
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(self.in_view.clone())),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.vp, &self.out_view, 0, &streams);
            let [mut s] = streams;
            std::mem::ManuallyDrop::drop(&mut s.pInputSurface);
            r?;
            Ok(())
        }
    }

    /// Copies the NV12 output to system memory (software-encoder path only).
    pub fn read_nv12(&mut self, gpu: &Gpu, dst: &mut Vec<u8>) -> Result<()> {
        unsafe {
            if self.staging.is_none() {
                let mut d = D3D11_TEXTURE2D_DESC::default();
                self.out.GetDesc(&mut d);
                d.Usage = D3D11_USAGE_STAGING;
                d.BindFlags = 0;
                d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                let mut s = None;
                gpu.device.CreateTexture2D(&d, None, Some(&mut s))?;
                self.staging = s;
            }
            let st = self.staging.as_ref().unwrap();
            gpu.ctx.CopyResource(st, &self.out);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.ctx.Map(st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let (w, h) = (self.size.0 as usize, self.size.1 as usize);
            let pitch = m.RowPitch as usize;
            dst.resize(w * h * 3 / 2, 0);
            let src = m.pData as *const u8;
            for y in 0..h {
                std::ptr::copy_nonoverlapping(src.add(y * pitch), dst.as_mut_ptr().add(y * w), w);
            }
            // UV plane follows the Y plane at `pitch * height` (D3D11 NV12 layout).
            let uv = src.add(pitch * h);
            for y in 0..h / 2 {
                std::ptr::copy_nonoverlapping(uv.add(y * pitch), dst.as_mut_ptr().add(w * h + y * w), w);
            }
            gpu.ctx.Unmap(st, 0);
            Ok(())
        }
    }
}

/// Sanity check used by the agent's pre-flight report.
pub fn video_processor_ok(gpu: &Gpu) -> bool {
    gpu.device.cast::<ID3D11VideoDevice>().is_ok()
}

pub fn ensure_size(size: (u32, u32)) -> Result<()> {
    if size.0 < 16 || size.1 < 16 {
        bail!("screen size {size:?} too small");
    }
    Ok(())
}
