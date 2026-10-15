//! MVP tracker eyes: once a second, copy the newest game client's window with `PrintWindow`
//! and OCR the message bar along its bottom edge, where the game announces MVP deaths and spawns.
//!
//! `PrintWindow(PW_RENDERFULLCONTENT)` asks the compositor for the window's own content, so it
//! works while other windows cover the game (not while it is minimized: Windows stops drawing it).
//! The game's power-saving mode darkens the whole picture after a while without input; the
//! watcher brightens such frames back before reading (`dim_gain`).
//! Nothing is sent to or read from the game process, and frames are never stored or uploaded.
//! An elevated game only lets an elevated Gamepulse copy its window.
//!
//! Stopping = dropping the `Watcher`; the thread ends within a second, so "off" costs no CPU.
use super::{
    capture::{last_error, Bitmap, MemoryDc, ScreenDc, SelectedBitmap},
    ocr::{self, Ocr},
};
use crate::{boss, GameProfile};
use image::{imageops, GrayImage, Luma};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{GetLastError, ERROR_ACCESS_DENIED, HWND, RECT},
    Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, GetDC, GetDIBits, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    },
    Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS, PW_CLIENTONLY},
    UI::WindowsAndMessaging::GetClientRect,
};

/// Not exported by the windows crate: render DirectX/composed content too (Windows 8.1+)
const PW_RENDERFULLCONTENT: u32 = 2;
const INTERVAL: Duration = Duration::from_secs(1);
/// The message bar is the bottom 25 px of a 1094×585 client area; scale with the window
const BAR_TOP: f32 = 560.0 / 585.0;
/// Padding around the text helps the recognizer with characters touching the edge
const PADDING: u32 = 12;
/// A game that is drawing never shows the exact same frame this long; when it does, Windows or
/// the game paused rendering and `PrintWindow` keeps returning the last frame
const STALE_AFTER: Duration = Duration::from_secs(10);
/// In power-saving mode nothing but the centered notice is brighter than this (white → ~105)
const DIM_MAX: u8 = 130;

pub enum Reading {
    /// OCR of the whole bar, plus of its green text when it mentions "MVP"
    Text { full: String, green: String },
    /// Reading state changed: None = reading the game window, Some = why it can't
    Problem(Option<String>),
    /// The game entered (Some(gain)) or left (None) its power-saving mode
    Dimmed(Option<f32>),
}

pub struct Watcher {
    stop: Arc<AtomicBool>,
    pub readings: Receiver<Reading>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(game: &'static GameProfile) -> Watcher {
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, readings) = mpsc::channel();
    let flag = stop.clone();
    thread::spawn(move || run(game, &flag, &sender));
    Watcher { stop, readings }
}

/// BGRA pixels of the message bar, top-down
struct Bar {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
    /// Hash of the whole client area, to notice a frozen window
    frame: u64,
    /// `dim_gain` the bar was brightened by (1 = normal frame)
    gain: f32,
}

impl Bar {
    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]]
    }

    /// White text pixels; the bar is empty most of the time, then there is nothing to read
    fn has_text(&self) -> bool {
        let bright = self
            .bgra
            .chunks_exact(4)
            .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
            .count();
        bright * 150 > (self.width * self.height) as usize
    }

    /// Dark-on-white image of the pixels `keep` selects
    fn mask(&self, keep: impl Fn([u8; 3]) -> bool) -> GrayImage {
        GrayImage::from_fn(self.width, self.height, |x, y| {
            Luma([if keep(self.pixel(x, y)) { 0 } else { 255 }])
        })
    }
}

/// Any light text (white names, yellow/green highlights) against the dark bar
fn is_text([r, g, b]: [u8; 3]) -> bool {
    r.max(g).max(b) > 125
}

/// Boss names in death announcements are green; team names are white
fn is_green([r, g, b]: [u8; 3]) -> bool {
    g > 100 && i32::from(g) > i32::from(r) + 40 && i32::from(g) > i32::from(b) + 30
}

/// Factor that undoes the game's power-saving mode, or 1 for a normal frame. That mode multiplies
/// every pixel by ~0.41 except a white "power saving" notice in the middle, so white text in the
/// bar comes out ~105 and fails every text threshold. A normal frame always has bright UI
/// (icons, names, numbers); a dimmed one has none outside the middle.
fn dim_gain(bgra: &[u8], width: usize, height: usize) -> f32 {
    let (left, right) = (width * 35 / 100, width * 65 / 100);
    let (top, bottom) = (height * 60 / 100, height * 80 / 100);
    let (mut bright, mut peak) = (0usize, 0u8);
    for (i, p) in bgra.chunks_exact(4).enumerate() {
        let (x, y) = (i % width, i / width);
        if (left..right).contains(&x) && (top..bottom).contains(&y) {
            continue;
        }
        let value = p[0].max(p[1]).max(p[2]);
        if value > DIM_MAX {
            bright += 1;
        } else {
            peak = peak.max(value);
        }
    }
    // A few stray bright pixels are fine; a near-black frame (loading) is not dimmed, just dark
    if bright * 10_000 > width * height || peak < 60 {
        1.0
    } else {
        255.0 / f32::from(peak.max(90))
    }
}

fn brighten(bgra: &mut [u8], gain: f32) {
    for p in bgra.chunks_exact_mut(4) {
        for c in &mut p[..3] {
            *c = (f32::from(*c) * gain).round().min(255.0) as u8;
        }
    }
}

fn fingerprint(image: &GrayImage) -> u64 {
    let mut hasher = DefaultHasher::new();
    image.as_raw().hash(&mut hasher);
    hasher.finish()
}

/// Upscale (the recognizer wants letters taller than the game draws them) and pad with white
fn for_ocr(image: &GrayImage, max_side: u32) -> GrayImage {
    let scale = 3
        .min((max_side.saturating_sub(2 * PADDING)) / image.width().max(1))
        .max(1);
    let big = imageops::resize(
        image,
        image.width() * scale,
        image.height() * scale,
        imageops::FilterType::CatmullRom,
    );
    let mut padded = GrayImage::from_pixel(
        big.width() + 2 * PADDING,
        big.height() + 2 * PADDING,
        Luma([255]),
    );
    imageops::replace(&mut padded, &big, i64::from(PADDING), i64::from(PADDING));
    padded
}

fn read_bar(hwnd: usize) -> Result<Bar, String> {
    let hwnd = HWND(hwnd as *mut _);
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }
        .map_err(|e| format!("game window is gone ({e})"))?;
    let (width, height) = (client.right, client.bottom);
    if width < 400 || height < 300 {
        return Err("game window is too small to read".into());
    }
    let pixel_bytes = width as usize * height as usize * 4;
    if pixel_bytes > crate::preview::MAX_PIXEL_BYTES {
        return Err("game window is too large".into());
    }
    let mut pixels = vec![0u8; pixel_bytes];
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
        let selected = SelectedBitmap {
            dc: &memory.0,
            previous,
        };
        let flags = PRINT_WINDOW_FLAGS(PW_CLIENTONLY.0 | PW_RENDERFULLCONTENT);
        if !PrintWindow(hwnd, memory.0, flags).as_bool() {
            return Err(if GetLastError() == ERROR_ACCESS_DENIED {
                "the game runs as administrator; run Gamepulse as administrator too".into()
            } else {
                last_error("Windows would not copy the game window")
            });
        }
        drop(selected);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
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
            return Err(last_error("GetDIBits did not return the game window"));
        }
    }
    if pixels
        .chunks_exact(4)
        .all(|p| p[0] < 8 && p[1] < 8 && p[2] < 8)
    {
        return Err("Windows returned a blank frame (is the game minimized?)".into());
    }
    let mut hasher = DefaultHasher::new();
    pixels.hash(&mut hasher);
    let frame = hasher.finish();
    let gain = dim_gain(&pixels, width as usize, height as usize);
    let top = ((height as f32) * BAR_TOP) as u32;
    let start = top as usize * width as usize * 4;
    let mut bgra = pixels.split_off(start);
    if gain > 1.0 {
        brighten(&mut bgra, gain);
    }
    Ok(Bar {
        width: width as u32,
        height: height as u32 - top,
        bgra,
        frame,
        gain,
    })
}

/// Newest client whose window is not minimized
fn pick_window(game: &'static GameProfile) -> Result<usize, String> {
    let windows = super::game_windows(game)?;
    if windows.is_empty() {
        return Err(format!("{} is not running", game.name));
    }
    windows
        .iter()
        .find(|w| !w.minimized)
        .map(|w| w.hwnd)
        .ok_or_else(|| {
            "every game window is minimized; leave one open behind your other windows".into()
        })
}

fn run(game: &'static GameProfile, stop: &AtomicBool, sender: &Sender<Reading>) {
    ocr::init_thread();
    let ocr = match Ocr::new() {
        Ok(ocr) => ocr,
        Err(error) => {
            let _ = sender.send(Reading::Problem(Some(error)));
            return;
        }
    };
    // Some(...) never equals the first state, so the first state is always reported
    let mut reported: Option<Option<String>> = None;
    let mut report = |problem: Option<String>| {
        if reported.as_ref() != Some(&problem) {
            reported = Some(problem.clone());
            sender.send(Reading::Problem(problem)).is_ok()
        } else {
            true
        }
    };
    let (mut target, mut round, mut last_fingerprint, mut last_text) =
        (None, 0u32, 0u64, String::new());
    // Last frame hash and when it last changed
    let (mut last_frame, mut changed) = (0u64, Instant::now());
    let mut dimmed = false;
    while !stop.load(Ordering::Relaxed) {
        // Re-pick every 10 s: a newer client may have opened, or this one closed/minimized
        if target.is_none() || round % 10 == 0 {
            let previous = target;
            target = pick_window(game).map_or_else(
                |error| {
                    report(Some(error));
                    None
                },
                Some,
            );
            if target != previous {
                changed = Instant::now();
            }
        }
        round = round.wrapping_add(1);
        if let Some(hwnd) = target {
            match read_bar(hwnd) {
                Ok(bar) => {
                    if (bar.gain > 1.0) != dimmed {
                        dimmed = !dimmed;
                        let gain = dimmed.then_some(bar.gain);
                        if sender.send(Reading::Dimmed(gain)).is_err() {
                            return;
                        }
                    }
                    if bar.frame != last_frame {
                        (last_frame, changed) = (bar.frame, Instant::now());
                    }
                    let stale = changed.elapsed() >= STALE_AFTER;
                    let problem = stale.then(|| {
                        format!(
                            "the game window stopped updating for {} s or more",
                            STALE_AFTER.as_secs()
                        )
                    });
                    if !report(problem) {
                        return;
                    }
                    if !stale && bar.has_text() {
                        let text = bar.mask(is_text);
                        let print = fingerprint(&text);
                        if print != last_fingerprint {
                            last_fingerprint = print;
                            match ocr.read(&for_ocr(&text, ocr.max_side())) {
                                Ok(full) if !full.trim().is_empty() && full != last_text => {
                                    let green = if boss::mentions_mvp(&full) {
                                        ocr.read(&for_ocr(&bar.mask(is_green), ocr.max_side()))
                                            .unwrap_or_default()
                                    } else {
                                        String::new()
                                    };
                                    last_text = full.clone();
                                    if sender.send(Reading::Text { full, green }).is_err() {
                                        return;
                                    }
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    report(Some(error));
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    if !report(Some(error)) {
                        return;
                    }
                    target = None;
                }
            }
        }
        thread::sleep(INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_pick_text_and_green_names() {
        // white, green, dark background
        let bgra = vec![230, 230, 230, 255, 70, 140, 50, 255, 8, 8, 10, 255];
        let bar = Bar {
            width: 3,
            height: 1,
            bgra,
            frame: 0,
            gain: 1.0,
        };
        assert_eq!(bar.mask(is_text).as_raw(), &[0, 0, 255]);
        assert_eq!(bar.mask(is_green).as_raw(), &[255, 0, 255]);
    }

    /// `width`×`height` frame of `value` grey, with a white notice in the middle
    fn frame(width: usize, height: usize, value: u8) -> Vec<u8> {
        let mut bgra = vec![value; width * height * 4];
        let middle = (height / 2 + height / 5) * width + width / 2;
        bgra[middle * 4..middle * 4 + 40].fill(255);
        bgra
    }

    #[test]
    fn power_saving_frames_are_brightened_back() {
        // Power-saving mode: white UI at 105, the centered notice still white
        let gain = dim_gain(&frame(100, 60, 105), 100, 60);
        assert!((gain - 255.0 / 105.0).abs() < 0.01);
        let mut bar = vec![105, 105, 105, 255, 20, 20, 20, 255];
        brighten(&mut bar, gain);
        assert_eq!(bar, [255, 255, 255, 255, 49, 49, 49, 255]);
        // Normal frame: bright UI everywhere
        assert_eq!(dim_gain(&frame(100, 60, 200), 100, 60), 1.0);
        // Loading screen: dark, not dimmed
        assert_eq!(dim_gain(&frame(100, 60, 10), 100, 60), 1.0);
    }

    #[test]
    fn ocr_image_is_upscaled_padded_and_bounded() {
        let image = GrayImage::from_pixel(1094, 25, Luma([255]));
        let big = for_ocr(&image, 4096);
        assert_eq!(big.dimensions(), (1094 * 3 + 24, 75 + 24));
        let bounded = for_ocr(&image, 2600);
        assert!(bounded.width() <= 2600 && bounded.height() == 2 * 25 + 24);
    }
}
