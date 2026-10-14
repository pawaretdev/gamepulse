//! MVP route: a small always-on-top window listing which MVP to go for first, then next,
//! by spawn time. It runs its own message loop on its own thread (like `pip`), so it keeps
//! updating while the main window is hidden to the tray; the Engine writes what to show into
//! the shared `RouteView` and the window repaints from it every second.
//!
//! Drag anywhere to move it; right-click for "ticked MVPs only / all MVPs" and Close.
use std::{
    cell::RefCell,
    mem::size_of,
    sync::{mpsc, Arc, Mutex, Once},
    thread,
    time::Duration,
};
use windows::core::{w, PCWSTR};
use windows::Win32::{
    Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
        CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect,
        FrameRect, GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_END_ELLIPSIS,
        DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, HDC, HFONT, HGDIOBJ, NULL_PEN,
        OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
    },
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentProcessId},
    UI::{
        HiDpi::{GetDpiForSystem, GetDpiForWindow},
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW,
            GetWindowThreadProcessId, IsWindow, KillTimer, LoadCursorW, PostMessageW,
            PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetTimer, SetWindowPos,
            ShowWindow, SystemParametersInfoW, TrackPopupMenu, TranslateMessage, HTCAPTION,
            IDC_ARROW, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, SPI_GETWORKAREA,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
            WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_NCHITTEST, WM_NCRBUTTONUP, WM_PAINT, WM_TIMER,
            WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

const CLASS: PCWSTR = w!("GamepulseRoute");
const TIMER: usize = 1;
const MENU_TICKED: usize = 1;
const MENU_ALL: usize = 2;
const MENU_CLOSE: usize = 3;
/// Sizes at 100% scaling
const WIDTH: i32 = 400;
const ROW: i32 = 26;
const HEADER: i32 = 40;
const PAD: i32 = 12;
const MARGIN: i32 = 24;

/// 0xRRGGBB
#[derive(Clone, Copy, Default)]
pub struct RouteColors {
    pub background: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
}

pub struct RouteLine {
    pub name: String,
    pub map: String,
    /// Right column, e.g. "22:39 · in 3m" or "UP 12m"
    pub when: String,
    /// Status dot, 0xRRGGBB
    pub color: u32,
}

#[derive(Default)]
pub struct RouteView {
    pub lines: Vec<RouteLine>,
    /// Set by the window's menu; the Engine saves it and rebuilds `lines`
    pub show_all: bool,
    pub colors: RouteColors,
    /// Shown when there are no lines
    pub empty: String,
}

pub type Shared = Arc<Mutex<RouteView>>;

thread_local! {
    // One route window per thread
    static VIEW: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

fn view() -> Option<Shared> {
    VIEW.with(|view| view.borrow().clone())
}

fn rgb(color: u32) -> COLORREF {
    // COLORREF is 0x00BBGGRR
    COLORREF(((color & 0xFF) << 16) | (color & 0xFF00) | ((color >> 16) & 0xFF))
}

fn scaled(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// Open the route window; returns its HWND for `is_open` / `close`
pub fn open(shared: Shared) -> Result<usize, String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        VIEW.with(|view| *view.borrow_mut() = Some(shared));
        let created = unsafe { create() };
        let ok = created.is_ok();
        let _ = sender.send(created);
        if ok {
            let mut message = MSG::default();
            unsafe {
                while GetMessageW(&mut message, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "the window did not start".to_string())?
}

/// Whether `hwnd` is still our window (closed handles can be reused by others)
pub fn is_open(hwnd: usize) -> bool {
    let hwnd = HWND(hwnd as *mut _);
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        IsWindow(Some(hwnd)).as_bool() && pid == GetCurrentProcessId()
    }
}

pub fn close(hwnd: usize) {
    if is_open(hwnd) {
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

fn height_for(lines: usize, dpi: u32) -> i32 {
    scaled(HEADER + ROW * lines.max(1) as i32 + PAD, dpi)
}

unsafe fn create() -> Result<usize, String> {
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        unsafe { RegisterClassExW(&class) };
    });
    let dpi = unsafe { GetDpiForSystem() };
    let lines = view().map_or(0, |v| v.lock().map_or(0, |v| v.lines.len()));
    let (width, height) = (scaled(WIDTH, dpi), height_for(lines, dpi));
    let mut work = RECT::default();
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut _ as _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    let margin = scaled(MARGIN, dpi);
    let hwnd = unsafe {
        CreateWindowExW(
            // NOACTIVATE: dragging it never steals focus from the game or the app you are in
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("MVP route"),
            WS_POPUP,
            work.right - width - margin,
            work.top + margin,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    unsafe {
        SetTimer(Some(hwnd), TIMER, 1000, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(hwnd.0 as usize)
}

/// Grow or shrink with the number of lines, keeping the top-left corner where the user put it
fn fit_height(hwnd: HWND) {
    let lines = view().map_or(0, |v| v.lock().map_or(0, |v| v.lines.len()));
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let height = height_for(lines, dpi);
    if client.bottom != height {
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                client.right,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
}

fn font(dpi: u32, size: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -scaled(size, dpi),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

fn text(dc: HDC, value: &str, mut rect: RECT, color: u32, format: DRAW_TEXT_FORMAT) {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        SetTextColor(dc, rgb(color));
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            format | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }
}

/// Draw into a memory bitmap first so the once-a-second refresh doesn't flicker
fn paint(hwnd: HWND) {
    let Some(shared) = view() else {
        return;
    };
    let Ok(view) = shared.lock() else {
        return;
    };
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let s = |v: i32| scaled(v, dpi);
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let colors = view.colors;
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let screen = BeginPaint(hwnd, &mut ps);
        let dc = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, client.right, client.bottom);
        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let background = CreateSolidBrush(rgb(colors.background));
        FillRect(dc, &client, background);
        let border = CreateSolidBrush(rgb(colors.border));
        FrameRect(dc, &client, border);
        SetBkMode(dc, TRANSPARENT);

        let bold = font(dpi, 14, 600);
        let regular = font(dpi, 13, 400);
        let small = font(dpi, 11, 400);
        let old_font = SelectObject(dc, HGDIOBJ(bold.0));
        let width = client.right;
        text(
            dc,
            "MVP route",
            RECT {
                left: s(PAD),
                top: s(6),
                right: width - s(PAD),
                bottom: s(HEADER - 6),
            },
            colors.text,
            DT_LEFT,
        );
        SelectObject(dc, HGDIOBJ(small.0));
        let mode = if view.show_all {
            "all MVPs"
        } else {
            "ticked MVPs"
        };
        text(
            dc,
            &format!("{mode} · right-click for options"),
            RECT {
                left: s(PAD),
                top: s(6),
                right: width - s(PAD),
                bottom: s(HEADER - 6),
            },
            colors.muted,
            DT_RIGHT,
        );

        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        if view.lines.is_empty() {
            SelectObject(dc, HGDIOBJ(regular.0));
            text(
                dc,
                &view.empty,
                RECT {
                    left: s(PAD),
                    top: s(HEADER),
                    right: width - s(PAD),
                    bottom: s(HEADER + ROW),
                },
                colors.muted,
                DT_LEFT,
            );
        }
        for (index, line) in view.lines.iter().enumerate() {
            let top = s(HEADER + ROW * index as i32);
            let row = |left: i32, right: i32| RECT {
                left: s(left),
                top,
                right: s(right),
                bottom: top + s(ROW),
            };
            SelectObject(dc, HGDIOBJ(small.0));
            text(
                dc,
                &format!("{}", index + 1),
                row(PAD, PAD + 18),
                colors.muted,
                DT_LEFT,
            );
            let dot = CreateSolidBrush(rgb(line.color));
            let old_brush = SelectObject(dc, HGDIOBJ(dot.0));
            let middle = top + s(ROW) / 2;
            let _ = Ellipse(dc, s(PAD + 22), middle - s(4), s(PAD + 30), middle + s(4));
            SelectObject(dc, old_brush);
            let _ = DeleteObject(HGDIOBJ(dot.0));
            SelectObject(dc, HGDIOBJ(bold.0));
            text(dc, &line.name, row(PAD + 38, 168), colors.text, DT_LEFT);
            SelectObject(dc, HGDIOBJ(regular.0));
            text(dc, &line.map, row(172, 286), colors.muted, DT_LEFT);
            text(dc, &line.when, row(286, WIDTH - PAD), line.color, DT_RIGHT);
        }

        SelectObject(dc, old_pen);
        SelectObject(dc, old_font);
        for object in [bold.0, regular.0, small.0, background.0, border.0] {
            let _ = DeleteObject(HGDIOBJ(object));
        }
        let _ = BitBlt(
            screen,
            0,
            0,
            client.right,
            client.bottom,
            Some(dc),
            0,
            0,
            SRCCOPY,
        );
        SelectObject(dc, old_bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn context_menu(hwnd: HWND) {
    let show_all = view().is_some_and(|v| v.lock().is_ok_and(|v| v.show_all));
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(!show_all),
            MENU_TICKED,
            w!("Ticked MVPs only"),
        );
        let _ = AppendMenuW(menu, MF_STRING | check(show_all), MENU_ALL, w!("All MVPs"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, w!("Close"));
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        // Without this the menu doesn't close when you click elsewhere
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            cursor.x,
            cursor.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        match command {
            MENU_TICKED | MENU_ALL => {
                if let Some(shared) = view() {
                    if let Ok(mut view) = shared.lock() {
                        view.show_all = command == MENU_ALL;
                    }
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            MENU_CLOSE => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Drag anywhere to move
        WM_NCHITTEST => LRESULT(HTCAPTION as isize),
        WM_NCRBUTTONUP => {
            context_menu(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            fit_height(hwnd);
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            LRESULT(0)
        }
        // Everything is painted in WM_PAINT from a memory bitmap
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_become_colorref() {
        assert_eq!(rgb(0x123456).0, 0x563412);
    }
}
