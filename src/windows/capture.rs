//! One-shot capture of the complete Windows virtual desktop.
//!
//! This intentionally does not inspect game windows, move focus, or restore
//! minimized applications. Every monitor is copied into one bounded BMP.
use crate::Snapshot;
use serde::Serialize;
use std::{path::Path, process::ExitCode, time::SystemTime, time::UNIX_EPOCH};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
    },
    UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

pub(super) struct ScreenDc(pub(super) HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(Some(HWND::default()), self.0);
        }
    }
}

pub(super) struct MemoryDc(pub(super) HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

pub(super) struct Bitmap(pub(super) HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}

pub(super) struct SelectedBitmap<'a> {
    pub(super) dc: &'a HDC,
    pub(super) previous: HGDIOBJ,
}
impl Drop for SelectedBitmap<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(*self.dc, self.previous);
        }
    }
}

#[derive(Serialize)]
struct DesktopCapture {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    captured_unix_ms: u128,
    client_count: usize,
    preview_file: Option<String>,
}

pub(super) fn last_error(action: &str) -> String {
    format!("{action}: {}", windows::core::Error::from_win32())
}

fn capture(
    snapshot: &Snapshot,
    directory: Option<&Path>,
) -> Result<(DesktopCapture, Vec<u8>), String> {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if width <= 0 || height <= 0 {
        return Err("Virtual desktop has invalid dimensions".into());
    }
    let pixel_bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("Virtual desktop size overflow")?;
    if pixel_bytes > crate::preview::MAX_PIXEL_BYTES {
        return Err(format!(
            "Virtual desktop needs {pixel_bytes} bytes; limit is {} bytes",
            crate::preview::MAX_PIXEL_BYTES
        ));
    }

    unsafe {
        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            return Err(last_error("GetDC failed"));
        }
        let memory = MemoryDc(CreateCompatibleDC(Some(screen.0)));
        if memory.0.is_invalid() {
            return Err(last_error("CreateCompatibleDC failed"));
        }
        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, width, height));
        if bitmap.0.is_invalid() {
            return Err(last_error("CreateCompatibleBitmap failed"));
        }
        let previous = SelectObject(memory.0, HGDIOBJ(bitmap.0 .0));
        if previous.is_invalid() {
            return Err(last_error("SelectObject failed"));
        }
        let _selected = SelectedBitmap {
            dc: &memory.0,
            previous,
        };
        let copy = |raster_operation| {
            BitBlt(
                memory.0,
                0,
                0,
                width,
                height,
                Some(screen.0),
                left,
                top,
                raster_operation,
            )
        };
        // CAPTUREBLT includes layered windows, but some desktop/session policies
        // deny that operation. Fall back to a regular full-desktop copy rather
        // than making screenshot support unusable in those environments.
        if copy(SRCCOPY | CAPTUREBLT).is_err() {
            copy(SRCCOPY).map_err(|e| format!("BitBlt failed: {e}"))?;
        }

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height requests top-down rows, matching preview::bmp.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; pixel_bytes];
        let rows = GetDIBits(
            memory.0,
            bitmap.0,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        if rows != height {
            return Err(last_error("GetDIBits did not return the complete desktop"));
        }

        let captured_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let bytes = crate::preview::bmp(width as u32, height as u32, &pixels)?;
        let preview_file = if let Some(directory) = directory {
            let path = directory.join(format!("gamepulse-desktop-{captured_unix_ms}.bmp"));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("Cannot create preview: {e}"))?;
            if let Err(e) = file.write_all(&bytes) {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Err(format!("Cannot write preview: {e}"));
            }
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        };
        Ok((
            DesktopCapture {
                left,
                top,
                width,
                height,
                captured_unix_ms,
                client_count: snapshot.clients.len(),
                preview_file,
            },
            bytes,
        ))
    }
}

pub fn desktop_bmp() -> Result<Vec<u8>, String> {
    let snapshot = Snapshot {
        schema_version: 2,
        sampled_unix_ms: 0,
        clients: vec![],
        errors: vec![],
    };
    capture(&snapshot, None).map(|(_, bytes)| bytes)
}

pub fn run(snapshot: &Snapshot, directory: Option<&Path>) -> ExitCode {
    if let Some(directory) = directory {
        if let Err(e) = std::fs::create_dir_all(directory) {
            eprintln!("Cannot create preview directory: {e}");
            return ExitCode::from(3);
        }
    }
    match capture(snapshot, directory) {
        Ok((result, _)) => match serde_json::to_string_pretty(&result) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Cannot serialize capture result: {e}");
                ExitCode::from(3)
            }
        },
        Err(e) => {
            eprintln!("Desktop capture unavailable: {e}");
            ExitCode::from(1)
        }
    }
}
