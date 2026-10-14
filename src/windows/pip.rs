//! Picture-in-picture: a small always-on-top window showing a live DWM thumbnail of a game
//! window, like a video player's mini player. DWM composites the image on the GPU, so no
//! pixels are copied into this process and nothing is stored or sent anywhere.
//! Windows stops rendering minimized windows, so while the game is minimized the PiP shows a
//! hint instead of a frozen frame. Each PiP runs its own message loop on its own thread, so it
//! keeps working while the main window is hidden to the tray.

use std::{
    cell::RefCell,
    mem::size_of,
    sync::{mpsc, Once},
    thread,
    time::Duration,
};
use windows::core::{w, PCWSTR};
use windows::Win32::{
    Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{
            DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
            DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
            DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
        },
        Gdi::{
            BeginPaint, DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, SetBkMode,
            SetTextColor, BLACK_BRUSH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBRUSH, PAINTSTRUCT,
            TRANSPARENT,
        },
    },
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentProcessId},
    UI::{
        HiDpi::{GetDpiForSystem, GetDpiForWindow},
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW,
            GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, KillTimer, LoadCursorW,
            PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetTimer,
            ShowWindow, SystemParametersInfoW, TrackPopupMenu, TranslateMessage, HTBOTTOM,
            HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
            IDC_ARROW, MF_STRING, MINMAXINFO, MSG, SPI_GETWORKAREA, SW_RESTORE, SW_SHOWNOACTIVATE,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
            WMSZ_BOTTOM, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT, WM_CLOSE, WM_DESTROY,
            WM_GETMINMAXINFO, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONUP,
            WM_PAINT, WM_SIZE, WM_SIZING, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
        },
    },
};

const CLASS: PCWSTR = w!("GamePulsePip");
const TIMER: usize = 1;
const MENU_BACK: usize = 1;
const MENU_CLOSE: usize = 2;
/// Width of a new PiP at 100% scaling; the height follows the game's aspect ratio
const START_WIDTH: i32 = 480;
const MARGIN: i32 = 24;
const RESIZE_BORDER: i32 = 8;

struct State {
    source: HWND,
    pid: u32,
    thumbnail: isize,
    /// Width / height of the game's client area, kept while it is minimized (0×0 then)
    aspect: f64,
    minimized: bool,
}

thread_local! {
    // One PiP window per thread, so its state needs no locking
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Open a PiP of `source` (a window of process `pid`); `slot` stacks several PiPs upward
/// from the bottom-right corner. Returns the PiP's HWND for `is_open` / `close`.
pub fn open(pid: u32, source: usize, title: &str, slot: usize) -> Result<usize, String> {
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let created = unsafe { create(pid, HWND(source as *mut _), &title, slot) };
        let ok = created.is_ok();
        let _ = sender.send(created);
        if ok {
            message_loop();
        }
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "the window did not start".to_string())?
}

/// Whether `hwnd` is still one of our PiP windows (closed handles can be reused by others)
pub fn is_open(hwnd: usize) -> bool {
    let hwnd = HWND(hwnd as *mut _);
    unsafe { IsWindow(Some(hwnd)).as_bool() && owner(hwnd) == GetCurrentProcessId() }
}

pub fn close(hwnd: usize) {
    if is_open(hwnd) {
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

fn owner(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn aspect_of(source: HWND) -> Option<f64> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(source, &mut rect) }.ok()?;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    (width > 0 && height > 0).then(|| f64::from(width) / f64::from(height))
}

fn scaled(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// Returns the PiP HWND as usize, since HWND can't cross threads
unsafe fn create(pid: u32, source: HWND, title: &[u16], slot: usize) -> Result<usize, String> {
    if owner(source) != pid {
        return Err("the game window has closed".into());
    }
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0),
            lpszClassName: CLASS,
            ..Default::default()
        };
        unsafe { RegisterClassExW(&class) };
    });

    let aspect = aspect_of(source).unwrap_or(16.0 / 9.0);
    let dpi = unsafe { GetDpiForSystem() };
    let width = scaled(START_WIDTH, dpi);
    let height = (f64::from(width) / aspect).round() as i32;
    let margin = scaled(MARGIN, dpi);
    let mut work = RECT::default();
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut _ as _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    let x = work.right - width - margin;
    let y = (work.bottom - (slot as i32 + 1) * (height + margin)).max(work.top);

    let hwnd = unsafe {
        CreateWindowExW(
            // NOACTIVATE: dragging the PiP never steals focus from the app you are working in
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            PCWSTR(title.as_ptr()),
            WS_POPUP | WS_THICKFRAME,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    let thumbnail = match unsafe { DwmRegisterThumbnail(hwnd, source) } {
        Ok(thumbnail) => thumbnail,
        Err(e) => {
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(e.to_string());
        }
    };
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            source,
            pid,
            thumbnail,
            aspect,
            minimized: unsafe { IsIconic(source) }.as_bool(),
        })
    });
    update_thumbnail(hwnd);
    unsafe {
        SetTimer(Some(hwnd), TIMER, 500, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(hwnd.0 as usize)
}

fn message_loop() {
    let mut message = MSG::default();
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Fit the game into the PiP keeping its aspect ratio (black bars only while resizing lags)
fn update_thumbnail(hwnd: HWND) {
    STATE.with(|state| {
        let state = state.borrow();
        let Some(state) = state.as_ref() else {
            return;
        };
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let (width, height) = (f64::from(client.right), f64::from(client.bottom));
        let (fit_width, fit_height) = if width / height > state.aspect {
            (height * state.aspect, height)
        } else {
            (width, width / state.aspect)
        };
        let left = ((width - fit_width) / 2.0).round() as i32;
        let top = ((height - fit_height) / 2.0).round() as i32;
        let properties = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY
                | DWM_TNP_OPACITY,
            rcDestination: RECT {
                left,
                top,
                right: left + fit_width.round() as i32,
                bottom: top + fit_height.round() as i32,
            },
            opacity: 255,
            fVisible: (!state.minimized).into(),
            fSourceClientAreaOnly: true.into(),
            ..Default::default()
        };
        let _ = unsafe { DwmUpdateThumbnailProperties(state.thumbnail, &properties) };
    });
}

/// Every 500 ms: close when the game window is gone, show the hint while it is minimized
fn watch_source(hwnd: HWND) {
    let (gone, changed) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return (false, false);
        };
        if owner(state.source) != state.pid {
            return (true, false);
        }
        let minimized = unsafe { IsIconic(state.source) }.as_bool();
        let aspect = aspect_of(state.source).unwrap_or(state.aspect);
        let changed = minimized != state.minimized || (aspect - state.aspect).abs() > 0.01;
        state.minimized = minimized;
        state.aspect = aspect;
        (false, changed)
    });
    // DestroyWindow re-enters window_proc (WM_DESTROY), so the borrow above must be released
    if gone {
        let _ = unsafe { DestroyWindow(hwnd) };
    } else if changed {
        update_thumbnail(hwnd);
        let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
    }
}

/// Restore the game if needed, bring it to the front and close the PiP (like "back to tab")
fn back_to_game(hwnd: HWND) {
    let source = STATE.with(|state| state.borrow().as_ref().map(|s| s.source));
    if let Some(source) = source {
        unsafe {
            if IsIconic(source).as_bool() {
                let _ = ShowWindow(source, SW_RESTORE);
            }
            let _ = SetForegroundWindow(source);
        }
    }
    let _ = unsafe { DestroyWindow(hwnd) };
}

fn context_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_BACK, w!("Back to game"));
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
            MENU_BACK => back_to_game(hwnd),
            MENU_CLOSE => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

/// No title bar: drag anywhere to move, drag the edges to resize
fn hit_test(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    let x = i32::from(lparam.0 as u16 as i16);
    let y = i32::from((lparam.0 >> 16) as u16 as i16);
    let mut rect = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
    let border = scaled(RESIZE_BORDER, unsafe { GetDpiForWindow(hwnd) });
    let left = x < rect.left + border;
    let right = x >= rect.right - border;
    let top = y < rect.top + border;
    let bottom = y >= rect.bottom - border;
    let code = match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, true, ..) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (.., true) => HTBOTTOM,
        _ => HTCAPTION,
    };
    LRESULT(code as isize)
}

/// Keep the game's aspect ratio while the user drags an edge or corner
fn keep_aspect(edge: u32, rect: &mut RECT) {
    let aspect = STATE.with(|state| state.borrow().as_ref().map_or(16.0 / 9.0, |s| s.aspect));
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    if edge == WMSZ_TOP || edge == WMSZ_BOTTOM {
        rect.right = rect.left + (f64::from(height) * aspect).round() as i32;
    } else {
        let height = (f64::from(width) / aspect).round() as i32;
        if edge == WMSZ_TOPLEFT || edge == WMSZ_TOPRIGHT {
            rect.top = rect.bottom - height;
        } else {
            rect.bottom = rect.top + height;
        }
    }
}

fn paint(hwnd: HWND) {
    let minimized = STATE.with(|state| state.borrow().as_ref().is_some_and(|s| s.minimized));
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        FillRect(dc, &paint.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        if minimized {
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            SetTextColor(dc, COLORREF(0x00DD_DDDD));
            SetBkMode(dc, TRANSPARENT);
            let mut text: Vec<u16> = "Game minimized. Double-click to restore it."
                .encode_utf16()
                .collect();
            DrawTextW(
                dc,
                &mut text,
                &mut client,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        let _ = EndPaint(hwnd, &paint);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // The whole window is client area: no frame or title bar, WS_THICKFRAME only enables resizing
        WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),
        WM_NCHITTEST => hit_test(hwnd, lparam),
        WM_SIZING => {
            // SAFETY: for WM_SIZING, lparam points to the window rect being dragged
            keep_aspect(wparam.0 as u32, unsafe { &mut *(lparam.0 as *mut RECT) });
            LRESULT(1)
        }
        WM_GETMINMAXINFO => {
            // SAFETY: for WM_GETMINMAXINFO, lparam points to a MINMAXINFO
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT { x: 160, y: 90 };
            LRESULT(0)
        }
        WM_SIZE => {
            update_thumbnail(hwnd);
            let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
            LRESULT(0)
        }
        WM_TIMER => {
            watch_source(hwnd);
            LRESULT(0)
        }
        // Handled here so a double-click on the "caption" doesn't try to maximize
        WM_NCLBUTTONDBLCLK => {
            back_to_game(hwnd);
            LRESULT(0)
        }
        WM_NCRBUTTONUP => {
            context_menu(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER) };
            if let Some(state) = STATE.with(|state| state.borrow_mut().take()) {
                let _ = unsafe { DwmUnregisterThumbnail(state.thumbnail) };
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::WMSZ_RIGHT;

    #[test]
    fn resizing_keeps_the_game_aspect_ratio() {
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                source: HWND::default(),
                pid: 0,
                thumbnail: 0,
                aspect: 16.0 / 9.0,
                minimized: false,
            })
        });
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        // right edge: width wins, height follows downward
        let mut r = rect(0, 0, 640, 100);
        keep_aspect(WMSZ_RIGHT, &mut r);
        assert_eq!(r, rect(0, 0, 640, 360));
        // bottom edge: height wins, width follows
        let mut r = rect(0, 0, 100, 180);
        keep_aspect(WMSZ_BOTTOM, &mut r);
        assert_eq!(r, rect(0, 0, 320, 180));
        // top corner: the bottom stays put, the top moves
        let mut r = rect(0, 500, 320, 1000);
        keep_aspect(WMSZ_TOPLEFT, &mut r);
        assert_eq!(r, rect(0, 820, 320, 1000));
    }
}
