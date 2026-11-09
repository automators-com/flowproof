//! The replica's window: the screen drawn with GDI on a monospace grid, the
//! frame degraded as Citrix would before it reaches the window, and keys
//! translated into terminal keys.

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GdiFlush, GetDC, GetTextMetricsW, InvalidateRect,
    PatBlt, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW, ANTIALIASED_QUALITY,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DSTINVERT, FF_MODERN, FIXED_PITCH, FW_NORMAL, HDC, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    SRCCOPY, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_F1, VK_F12,
    VK_HOME, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect,
    GetMessageW, LoadCursorW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage,
    CW_USEDEFAULT, IDC_ARROW, MSG, WINDOW_EX_STYLE, WM_CHAR, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_PAINT, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::screen::{Key, Screen};
use crate::Options;

const MARGIN: i32 = 10;

struct App {
    screen: Screen,
    opts: Options,
    font: HFONT,
    cell: (i32, i32),
    was_busy: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn run(screen: Screen, opts: Options) -> Result<(), String> {
    unsafe {
        // Physical pixels, so Windows never rescales the frame behind our back.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("FlowproofTerminalReplica");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err("RegisterClassW failed".into());
        }

        let face = HSTRING::from(screen.layout.font.as_deref().unwrap_or("Consolas"));
        let font = CreateFontW(
            -screen.layout.font_px.unwrap_or(18),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            (FIXED_PITCH.0 as u32) | (FF_MODERN.0 as u32),
            &face,
        );
        let dc = GetDC(None);
        let old = SelectObject(dc, font.into());
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        ReleaseDC(None, dc);
        let cell = (tm.tmAveCharWidth, tm.tmHeight);

        let (cols, rows) = (screen.layout.cols as i32, screen.layout.rows as i32);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: cols * cell.0 + 2 * MARGIN,
            bottom: rows * cell.1 + 3 * MARGIN + cell.1 + 8,
        };
        let _ = AdjustWindowRectEx(&mut rect, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0));
        let title = HSTRING::from(screen.layout.title.as_str());

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                screen,
                opts,
                font,
                cell,
                was_busy: false,
            })
        });
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &title,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        SetTimer(Some(hwnd), 1, 100, None);
        with_app(|app| app.save());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

impl App {
    /// Write the ground truth atomically, so a scorer never reads half a file.
    fn save(&self) {
        let Some(path) = &self.opts.state else { return };
        let state = self.screen.state(Instant::now());
        let tmp = path.with_extension("tmp");
        let text = serde_json::to_string_pretty(&state).unwrap_or_default();
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_KEYDOWN | WM_SYSKEYDOWN => match key_down(VIRTUAL_KEY(wp.0 as u16)) {
            Some(key) => {
                handle(hwnd, key);
                LRESULT(0)
            }
            None => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wp.0 as u32).filter(|c| !c.is_control()) {
                handle(hwnd, Key::Char(ch));
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let changed = with_app(|app| {
                let busy = app.screen.busy(Instant::now());
                let changed = busy != app.was_busy;
                app.was_busy = busy;
                if changed {
                    app.save();
                }
                changed
            });
            if changed == Some(true) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn handle(hwnd: HWND, key: Key) {
    with_app(|app| {
        app.screen.press(key, Instant::now());
        app.save();
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn key_down(vk: VIRTUAL_KEY) -> Option<Key> {
    let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
    if down(VK_MENU) {
        return None; // Alt chords (Alt+F4) belong to Windows
    }
    let (shift, ctrl) = (down(VK_SHIFT), down(VK_CONTROL));
    let name = match vk {
        VK_END => "End",
        VK_HOME => "Home",
        VK_DELETE => "Delete",
        VK_BACK => "Backspace",
        _ => "",
    };
    if !name.is_empty() {
        let chord = format!(
            "{}{}{name}",
            if ctrl { "Ctrl+" } else { "" },
            if shift { "Shift+" } else { "" }
        );
        let erase = with_app(|app| chord.eq_ignore_ascii_case(&app.screen.layout.keys.erase_eof));
        if erase == Some(true) {
            return Some(Key::EraseEof);
        }
    }
    Some(match vk {
        VK_TAB if shift => Key::BackTab,
        VK_TAB => Key::Tab,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_RETURN => Key::Enter,
        _ if (VK_F1.0..=VK_F12.0).contains(&vk.0) => {
            let n = (vk.0 - VK_F1.0 + 1) as u8;
            Key::Pf(if shift { n + 12 } else { n })
        }
        _ => return None,
    })
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (w, h) = (rc.right.max(1), rc.bottom.max(1));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mem = CreateCompatibleDC(Some(hdc));
        let mut bits: *mut c_void = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(mem, dib.into());
            with_app(|app| {
                draw(mem, &rc, app);
                let _ = GdiFlush();
                if !app.opts.degrade.is_clean() && !bits.is_null() {
                    let len = (w * h * 4) as usize;
                    let buf = std::slice::from_raw_parts_mut(bits.cast::<u8>(), len);
                    if let Err(e) = app.opts.degrade.apply(buf, w as u32, h as u32) {
                        eprintln!("degrade: {e}");
                    }
                }
            });
            let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(dib.into());
        }
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Fill a rectangle with a solid colour.
unsafe fn fill(dc: HDC, rect: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    FillRect(dc, &rect, brush);
    let _ = DeleteObject(brush.into());
}

unsafe fn draw(dc: HDC, rc: &RECT, app: &App) {
    let (cw, ch) = app.cell;
    let at = |row: usize, col: usize| (MARGIN + col as i32 * cw, MARGIN + row as i32 * ch);
    let layout = &app.screen.layout;
    fill(dc, *rc, rgb(240, 240, 240));
    SelectObject(dc, app.font.into());
    let _ = SetBkMode(dc, TRANSPARENT);

    // 0 = protected text, 1 = highlighted text, 2 = input field.
    let mut kind = vec![vec![0u8; layout.cols]; layout.rows];
    for l in layout.text.iter().filter(|l| l.blue) {
        let end = (l.col + l.text.chars().count()).min(layout.cols);
        kind[l.row][l.col..end].fill(1);
    }
    for (row, col, len) in app.screen.field_rects() {
        let (x, y) = at(row, col);
        fill(
            dc,
            RECT {
                left: x,
                top: y,
                right: x + len as i32 * cw,
                bottom: y + ch,
            },
            rgb(255, 255, 255),
        );
        kind[row][col..col + len].fill(2);
    }
    for (row, line) in app.screen.grid().iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            let k = kind[row][start];
            let end = (start..chars.len())
                .find(|&c| kind[row][c] != k)
                .unwrap_or(chars.len());
            let run: Vec<u16> = chars[start..end]
                .iter()
                .collect::<String>()
                .encode_utf16()
                .collect();
            let _ = SetTextColor(dc, if k == 0 { rgb(0, 0, 0) } else { rgb(0, 0, 205) });
            let (x, y) = at(row, start);
            let _ = TextOutW(dc, x, y, &run);
            start = end;
        }
    }
    let (x, y) = at(app.screen.cursor.0, app.screen.cursor.1);
    let _ = PatBlt(dc, x, y + ch - 2, cw, 2, DSTINVERT);

    let top = 2 * MARGIN + layout.rows as i32 * ch;
    fill(
        dc,
        RECT {
            left: 0,
            top,
            right: rc.right,
            bottom: rc.bottom,
        },
        rgb(225, 225, 225),
    );
    let _ = SetTextColor(dc, rgb(0, 0, 0));
    let status: Vec<u16> = app.screen.status(Instant::now()).encode_utf16().collect();
    let _ = TextOutW(dc, MARGIN, top + 4, &status);
}
