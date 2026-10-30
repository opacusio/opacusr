//! Windows backend: DXGI Desktop Duplication.
//!
//! Grabs frames straight out of the DWM composition — no GDI blit, works
//! with hardware overlays and fullscreen exclusive content that GDI misses.
//! Captures one output (monitor); the list comes from the DXGI adapter
//! enumeration.
//!
//! Desktop duplication only produces frames *on change*: when the desktop
//! is idle `AcquireNextFrame` times out and the backend re-serves the last
//! captured frame, which is exactly what a screen-sharing stream wants.
//!
//! Requirements: an interactive desktop session (not a Windows service).
//! [`platform_capture`](crate::platform_capture) falls back to GDI when
//! initialization or duplication fails.

#![cfg(windows)]

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
};

use crate::source::{ScreenCapture, ScreenDesc};
use crate::{bgra_to_rgba, CaptureError, Frame};

/// One DXGI output (monitor) on one adapter, flattened into a stable list.
#[derive(Debug, Clone)]
struct DxgiOutput {
    name: String,
    /// `true` when the monitor contains the origin (taskbar / primary).
    primary: bool,
    w: u32,
    h: u32,
}

pub struct DxgiCapture {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    outputs: Vec<DxgiOutput>,
    selected: usize,
    /// Active duplication + the dimensions of the last acquired frame.
    duplication: Option<(IDXGIOutputDuplication, u32, u32)>,
    /// Last successfully captured frame (re-served on idle timeouts).
    last: Option<Frame>,
}

// SAFETY: all fields are COM interface pointers (refcounted, free-threaded
// where we use them) or plain data. The session loop uses the capture from
// one thread at a time, and every GDI/DXGI resource used in a call is
// acquired and released within that call.
unsafe impl Send for DxgiCapture {}

/// Enumerate adapters/outputs with the same flattening everywhere.
fn enumerate_outputs() -> Result<Vec<(IDXGIOutput, DxgiOutput)>, CaptureError> {
    let err = |e: windows::core::Error| CaptureError::Win32(format!("dxgi: {e}"));
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(err)?;
    let mut out = Vec::new();
    let mut adapter_i = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(adapter_i) } {
            Ok(a) => a,
            Err(_) => break, // DXGI_ERROR_NOT_FOUND → done
        };
        let mut output_j = 0u32;
        loop {
            let output: IDXGIOutput = match unsafe { adapter.EnumOutputs(output_j) } {
                Ok(o) => o,
                Err(_) => break,
            };
            let desc = unsafe { output.GetDesc() }.map_err(err)?;
            if desc.AttachedToDesktop.as_bool() {
                let name = String::from_utf16_lossy(
                    &desc.DeviceName[..desc.DeviceName.iter().position(|&c| c == 0).unwrap_or(32)],
                );
                // Primary = the output whose desktop coordinates contain (0,0).
                let rc = &desc.DesktopCoordinates;
                let primary = rc.left <= 0 && rc.top <= 0 && rc.right > 0 && rc.bottom > 0;
                out.push((
                    output,
                    DxgiOutput {
                        name,
                        primary,
                        w: (rc.right - rc.left).max(0) as u32,
                        h: (rc.bottom - rc.top).max(0) as u32,
                    },
                ));
            }
            output_j += 1;
        }
        adapter_i += 1;
    }
    // Primary first for stable ids (mirrors the GDI backend).
    out.sort_by(|a, b| b.1.primary.cmp(&a.1.primary).then(a.1.name.cmp(&b.1.name)));
    Ok(out)
}

impl DxgiCapture {
    /// Create the device and enumerate outputs. Duplication starts lazily
    /// on the first `capture()`.
    pub fn new() -> Result<Self, CaptureError> {
        let listed = enumerate_outputs()?;
        if listed.is_empty() {
            return Err(CaptureError::Win32(
                "dxgi: no outputs attached to the desktop".into(),
            ));
        }
        let outputs: Vec<DxgiOutput> = listed.into_iter().map(|(_, d)| d).collect();
        let (device, context) = Self::create_device()?;
        Ok(Self {
            device,
            context,
            outputs,
            selected: 0,
            duplication: None,
            last: None,
        })
    }

    fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
        let err = |e: windows::core::Error| CaptureError::Win32(format!("d3d11: {e}"));
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let levels: [D3D_FEATURE_LEVEL; 1] = [D3D_FEATURE_LEVEL_11_0];
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(err)?;
        Ok((
            device.ok_or_else(|| CaptureError::Win32("d3d11: no device".into()))?,
            context.ok_or_else(|| CaptureError::Win32("d3d11: no context".into()))?,
        ))
    }

    /// (Re-)create the duplication for the selected output.
    fn ensure_duplication(&mut self) -> Result<(), CaptureError> {
        if self.duplication.is_some() {
            return Ok(());
        }
        let err = |e: windows::core::Error| CaptureError::Win32(format!("dxgi duplicate: {e}"));
        let listed = enumerate_outputs()?;
        let (output, _) = listed
            .into_iter()
            .nth(self.selected)
            .ok_or_else(|| CaptureError::Win32("dxgi: output vanished".into()))?;
        let output1: IDXGIOutput1 = output.cast().map_err(err)?;
        let duplication: IDXGIOutputDuplication =
            unsafe { output1.DuplicateOutput(&self.device) }.map_err(err)?;
        self.duplication = Some((duplication, 0, 0));
        Ok(())
    }

    fn drop_duplication(&mut self) {
        self.duplication = None;
    }
}

impl ScreenCapture for DxgiCapture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        self.ensure_duplication()?;
        // Clone the interface so the mutable borrow of `self` ends here.
        let dup = self.duplication.as_ref().unwrap().0.clone();

        // Acquire with a short timeout: no change → re-serve the cached frame.
        let timeout_ms = 66u32;
        let mut info = unsafe { std::mem::zeroed() };
        let mut resource: Option<IDXGIResource> = None;
        let acquired = unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) };

        let new_tex: Option<ID3D11Texture2D> = match acquired {
            Ok(()) => match resource.as_ref().map(|r| r.cast::<ID3D11Texture2D>()) {
                Some(Ok(t)) => Some(t),
                Some(Err(e)) => {
                    unsafe {
                        let _ = dup.ReleaseFrame();
                    }
                    return Err(CaptureError::Win32(format!(
                        "dxgi: frame texture cast: {e}"
                    )));
                }
                None => None,
            },
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => None,
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                // Switched desktop, UAC prompt, mode change — rebuild next call.
                self.drop_duplication();
                return self.last.clone().ok_or(CaptureError::Resized);
            }
            Err(e) => return Err(CaptureError::Win32(format!("dxgi acquire: {e}"))),
        };

        let Some(tex) = new_tex else {
            // Idle desktop: repeat the last frame so the stream keeps a
            // steady fps without new DWM work.
            return self.last.clone().ok_or(CaptureError::Resized);
        };

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let (w, h) = (desc.Width, desc.Height);

        // GPU texture → CPU staging buffer → RGBA.
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
        };
        let mut staging_opt: Option<ID3D11Texture2D> = None;
        unsafe {
            self.device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_opt))
        }
        .map_err(|e| CaptureError::Win32(format!("dxgi staging: {e}")))?;
        let staging: ID3D11Texture2D =
            staging_opt.ok_or_else(|| CaptureError::Win32("dxgi: no staging texture".into()))?;
        unsafe { self.context.CopyResource(&staging, &tex) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|e| CaptureError::Win32(format!("dxgi map: {e}")))?;
        let row_pitch = mapped.RowPitch as usize;
        let src: &[u8] = unsafe {
            std::slice::from_raw_parts(mapped.pData as *const u8, row_pitch * h as usize)
        };
        let stride = w as usize * 4;
        let mut rgba = Vec::with_capacity(stride * h as usize);
        for row in src.chunks_exact(row_pitch) {
            rgba.extend_from_slice(&row[..stride]);
        }
        unsafe { self.context.Unmap(&staging, 0) };
        unsafe {
            let _ = dup.ReleaseFrame();
        }

        bgra_to_rgba(&mut rgba);
        if let Some((_, lw, lh)) = self.duplication.as_mut() {
            *lw = w;
            *lh = h;
        }
        let frame = Frame::new(w, h, rgba);
        self.last = Some(frame.clone());
        Ok(frame)
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        if let Some((_, w, h)) = &self.duplication {
            if *w > 0 {
                return Ok((*w, *h));
            }
        }
        // Fall back to the desktop coordinates of the selected output.
        let d = self
            .outputs
            .get(self.selected)
            .ok_or(CaptureError::Win32("dxgi: no output".into()))?;
        if d.w > 0 {
            Ok((d.w, d.h))
        } else {
            Err(CaptureError::Win32("dxgi: unknown output size".into()))
        }
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        self.outputs
            .iter()
            .enumerate()
            .map(|(i, o)| ScreenDesc {
                id: i as u32,
                name: if o.primary {
                    format!("{} (primary)", o.name)
                } else {
                    o.name.clone()
                },
                w: o.w,
                h: o.h,
            })
            .collect()
    }

    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        let idx = id as usize;
        if idx >= self.outputs.len() {
            return Err(CaptureError::Win32(format!(
                "unknown screen id {id} ({} outputs)",
                self.outputs.len()
            )));
        }
        let changed = idx != self.selected;
        if changed {
            self.drop_duplication();
            self.selected = idx;
            // Warm up so size() and the first frame work immediately.
            self.ensure_duplication()?;
        }
        Ok(changed)
    }

    fn backend(&self) -> &'static str {
        "dxgi"
    }
}
