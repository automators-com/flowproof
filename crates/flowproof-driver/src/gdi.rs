//! Windows screen capture via GDI BitBlt. Deliberately simple v1 keyframe
//! capture; DXGI desktop duplication replaces it for continuous recording.

#![cfg(windows)]

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::DriverError;

fn gdi_err(context: &str) -> DriverError {
    DriverError::Capture(format!("GDI {context} failed"))
}

/// Capture the primary screen as RGBA.
pub fn capture_screen() -> Result<image::RgbaImage, DriverError> {
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    capture_region(0, 0, width, height)
}

/// The virtual desktop - every monitor - as `(x, y, width, height)` in
/// screen coordinates. A monitor left of or above the primary one has
/// negative coordinates.
pub fn virtual_desktop() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Capture a rectangle of the virtual desktop, in screen coordinates, as
/// RGBA. The screen DC spans every monitor, so a window on a secondary one
/// - negative coordinates included - is captured where it is; anything
/// outside the desktop reads black. Plan 15, finding 3: vision captured
/// the primary screen and cropped, so a Citrix window on a second monitor
/// was captured wrongly and clicked somewhere else.
pub fn capture_region(
    left: i32,
    top: i32,
    width: i32,
    height: i32,
) -> Result<image::RgbaImage, DriverError> {
    unsafe {
        if width <= 0 || height <= 0 {
            return Err(gdi_err("screen metrics"));
        }

        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err(gdi_err("GetDC"));
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        let old = SelectObject(mem_dc, bitmap.into());

        let result = (|| {
            BitBlt(
                mem_dc,
                0,
                0,
                width,
                height,
                Some(screen_dc),
                left,
                top,
                SRCCOPY | CAPTUREBLT,
            )
            .map_err(|_| gdi_err("BitBlt"))?;

            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
            let copied = GetDIBits(
                mem_dc,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            );
            if copied == 0 || copied == E_FAIL.0 {
                return Err(gdi_err("GetDIBits"));
            }
            // BGRA -> RGBA, force opaque alpha.
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
            }
            image::RgbaImage::from_raw(width as u32, height as u32, pixels)
                .ok_or_else(|| gdi_err("buffer size"))
        })();

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
        result
    }
}

/// Make this process per-monitor DPI aware, so window rectangles, captures
/// and input coordinates are all physical pixels. A DPI-unaware process at
/// 125% or 150% scaling is handed scaled window rectangles while its
/// screen captures are physical, so the two disagree and clicks land off
/// target (plan 15, finding 4). Idempotent: a second call - or a manifest
/// that already declared awareness - is refused by Windows and ignored.
pub fn ensure_dpi_aware() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(test)]
mod tests {
    /// Runs on the Windows dev machine and the Windows CI job: a window's
    /// rectangle can be captured anywhere on the desktop, and awareness
    /// can be asked for twice.
    #[test]
    fn a_region_of_the_desktop_captures_at_its_own_size() {
        super::ensure_dpi_aware();
        super::ensure_dpi_aware();
        let (x, y, w, h) = super::virtual_desktop();
        assert!(w > 0 && h > 0);
        let image = super::capture_region(x + 10, y + 10, 64, 32).expect("captures");
        assert_eq!(image.dimensions(), (64, 32));
    }
}
