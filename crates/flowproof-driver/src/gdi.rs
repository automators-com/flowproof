//! Windows screen capture via GDI BitBlt. Deliberately simple v1 keyframe
//! capture; DXGI desktop duplication replaces it for continuous recording.

#![cfg(windows)]

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

use crate::DriverError;

fn gdi_err(context: &str) -> DriverError {
    DriverError::Capture(format!("GDI {context} failed"))
}

/// Capture the primary screen as RGBA.
pub fn capture_screen() -> Result<image::RgbaImage, DriverError> {
    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
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
                0,
                0,
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
