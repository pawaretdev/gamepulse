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
/// Event › MVP window, as fractions of the client area (measured on 1094×585): the list's
/// white left margin, the list of boss cards, and the rows' vertical extent
const PANEL_MARGIN: (f32, f32) = (235.0 / 1094.0, 245.0 / 1094.0);
const PANEL_LIST: (f32, f32) = (247.0 / 1094.0, 537.0 / 1094.0);
const PANEL_ROWS: (f32, f32) = (134.0 / 585.0, 551.0 / 585.0);
/// The countdown column, between the clock icon and the crown
const PANEL_TIMES: (f32, f32) = (342.0 / 1094.0, 418.0 / 1094.0);
/// Look for the MVP window in every client this often (rounds of `INTERVAL`)
const PANEL_EVERY: u32 = 2;

pub enum Reading {
    /// OCR of the whole bar, plus of its green text when it mentions "MVP"
    Text { full: String, green: String },
    /// Reading state changed: None = reading the game window, Some = why it can't
    Problem(Option<String>),
    /// The game entered (Some(gain)) or left (None) its power-saving mode
    Dimmed(Option<f32>),
    /// The game's Event › MVP window, while one of the clients shows it: OCR lines of its list
    /// and of its countdown column, and where that column has red text, all as tops in percent
    /// of the list height
    Panel {
        names: Vec<(f32, String)>,
        times: Vec<(f32, String)>,
        /// The countdown column read again at another scale
        check: Vec<(f32, String)>,
        red: Vec<f32>,
    },
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
    scaled_for_ocr(image, max_side, 3)
}

/// `for_ocr` at most `scale` times; a second scale makes different mistakes, so two readings
/// that agree can be trusted where one can't
fn scaled_for_ocr(image: &GrayImage, max_side: u32, scale: u32) -> GrayImage {
    let scale = scale
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

/// One copy of a game window's client area, BGRA top-down
struct Frame {
    width: usize,
    height: usize,
    bgra: Vec<u8>,
}

impl Frame {
    /// The message bar along the bottom, brightened back when the game is in power-saving mode
    fn bar(&self) -> Bar {
        let mut hasher = DefaultHasher::new();
        self.bgra.hash(&mut hasher);
        let frame = hasher.finish();
        let gain = dim_gain(&self.bgra, self.width, self.height);
        let top = ((self.height as f32) * BAR_TOP) as usize;
        let mut bgra = self.bgra[top * self.width * 4..].to_vec();
        if gain > 1.0 {
            brighten(&mut bgra, gain);
        }
        Bar {
            width: self.width as u32,
            height: (self.height - top) as u32,
            bgra,
            frame,
            gain,
        }
    }

    /// Share of near-white pixels in a rectangle given as fractions of the client area
    fn light_share(&self, (left, right): (f32, f32), (top, bottom): (f32, f32)) -> f32 {
        let x = |f: f32| (f * self.width as f32) as usize;
        let y = |f: f32| (f * self.height as f32) as usize;
        let (mut light, mut total) = (0usize, 0usize);
        for row in (y(top)..y(bottom)).step_by(2) {
            for column in (x(left)..x(right)).step_by(2) {
                let i = (row * self.width + column) * 4;
                let p = &self.bgra[i..i + 3];
                total += 1;
                if p.iter().all(|&c| c > 225) {
                    light += 1;
                }
            }
        }
        light as f32 / total.max(1) as f32
    }

    /// A rectangle given as fractions of the client area, each RGB pixel mapped to grey by `shade`
    fn region(
        &self,
        (left, right): (f32, f32),
        (top, bottom): (f32, f32),
        shade: impl Fn([u8; 3]) -> u8,
    ) -> GrayImage {
        let x0 = (left * self.width as f32) as u32;
        let y0 = (top * self.height as f32) as u32;
        let width = (right * self.width as f32) as u32 - x0;
        let height = (bottom * self.height as f32) as u32 - y0;
        GrayImage::from_fn(width, height, |x, y| {
            let i = (((y0 + y) as usize) * self.width + (x0 + x) as usize) * 4;
            Luma([shade([self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]])])
        })
    }
}

fn capture(hwnd: usize) -> Result<Frame, String> {
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
    Ok(Frame {
        width: width as usize,
        height: height as usize,
        bgra: pixels,
    })
}

/// Whether the game's Event › MVP window is open: its list has a white left margin and white
/// cards (measured: 94% and 71% near-white; 0% on a normal or dimmed game screen)
fn shows_mvp_panel(frame: &Frame) -> bool {
    frame.light_share(PANEL_MARGIN, PANEL_ROWS) >= 0.8
        && frame.light_share(PANEL_LIST, PANEL_ROWS) >= 0.5
}

fn luma([r, g, b]: [u8; 3]) -> u8 {
    ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000) as u8
}

/// The countdown ("47:21") and "refreshed" text in the MVP list are pinkish red (~210,85,120):
/// dark where a pixel is red, keeping the anti-aliased edges (a hard mask turns "1" into "T")
fn redness([r, g, b]: [u8; 3]) -> u8 {
    let red = (i32::from(r) - i32::from(g.max(b))).max(0) * 3;
    255 - red.min(255) as u8
}

/// OCR lines of `image` upscaled up to `scale` times, each with its top as a percentage of the
/// image height
fn lines_at(ocr: &Ocr, image: &GrayImage, scale: u32) -> Option<Vec<(f32, String)>> {
    let big = scaled_for_ocr(image, ocr.max_side(), scale);
    let scale = (big.width() - 2 * PADDING) as f32 / image.width() as f32;
    let lines = ocr.read_lines(&big).ok()?;
    Some(
        lines
            .into_iter()
            .map(|line| {
                let top = (line.top - PADDING as f32) / scale / image.height() as f32 * 100.0;
                (top, line.text)
            })
            .collect(),
    )
}

/// The MVP window's list: OCR lines of the whole list and of the countdown column (read from a
/// redness image, twice at different scales: a lone reading turns 27 into 21 now and then),
/// tagged with their height in the list in percent. None unless at least two lines are map
/// names (otherwise it was something else that looked white)
fn read_mvp_panel(ocr: &Ocr, mvps: &'static [boss::Mvp], frame: &Frame) -> Option<Reading> {
    let names = lines_at(ocr, &frame.region(PANEL_LIST, PANEL_ROWS, luma), 3)?;
    let maps = names
        .iter()
        .filter(|(_, text)| boss::map_named(mvps, text).is_some())
        .count();
    if maps < 2 {
        return None;
    }
    let red = frame.region(PANEL_TIMES, PANEL_ROWS, redness);
    let times = lines_at(ocr, &red, 3).unwrap_or_default();
    let check = lines_at(ocr, &red, 2).unwrap_or_default();
    Some(Reading::Panel {
        names,
        times,
        check,
        red: red_text_tops(&red),
    })
}

/// Tops (percent of the image height) of the runs of rows that hold red text, so a row whose
/// countdown OCR can't read is told apart from a row with nothing red in it yet (the window
/// is still opening) or with the Thai "refreshed" text that OCR doesn't read at all
fn red_text_tops(image: &GrayImage) -> Vec<f32> {
    let height = image.height();
    let red_row = |y: u32| {
        (0..image.width())
            .filter(|&x| image.get_pixel(x, y).0[0] < 128)
            .count()
            >= 3
    };
    let mut tops = vec![];
    let mut y = 0;
    while y < height {
        if red_row(y) {
            let start = y;
            while y < height && red_row(y) {
                y += 1;
            }
            // Text is at least a few pixels tall; single lines are stray pixels
            if y - start >= 4 {
                tops.push(start as f32 / height as f32 * 100.0);
            }
        }
        y += 1;
    }
    tops
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
        // This round's copy of the target window, reused when looking for the MVP window
        let mut current: Option<(usize, Frame)> = None;
        if let Some(hwnd) = target {
            match capture(hwnd) {
                Ok(frame) => {
                    let bar = frame.bar();
                    current = Some((hwnd, frame));
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
        // The MVP window can be open in any client, not only the one whose bar is read
        if round % PANEL_EVERY == 0 {
            for window in super::game_windows(game)
                .unwrap_or_default()
                .iter()
                .filter(|w| !w.minimized)
            {
                let frame = match current.take() {
                    Some((hwnd, frame)) if hwnd == window.hwnd => Some(frame),
                    other => {
                        current = other;
                        capture(window.hwnd).ok()
                    }
                };
                let panel = frame
                    .filter(shows_mvp_panel)
                    .and_then(|frame| read_mvp_panel(&ocr, game.mvps, &frame));
                if let Some(panel) = panel {
                    if sender.send(panel).is_err() {
                        return;
                    }
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
    fn mvp_window_is_recognized_by_its_white_list() {
        let (width, height) = (1094, 585);
        let mut frame = Frame {
            width,
            height,
            bgra: vec![90; width * height * 4],
        };
        assert!(!shows_mvp_panel(&frame));
        // White margin and cards where the game draws the list
        for y in 134..551 {
            for x in 235..537 {
                let i = (y * width + x) * 4;
                frame.bgra[i..i + 3].fill(250);
            }
        }
        assert!(shows_mvp_panel(&frame));
    }

    #[test]
    fn countdown_red_becomes_dark_and_the_rest_white() {
        assert!(redness([210, 85, 120]) < 30);
        assert_eq!(redness([250, 250, 250]), 255);
        assert_eq!(redness([40, 50, 90]), 255);
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
