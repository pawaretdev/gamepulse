//! Text recognition with the OCR engine built into Windows (`Windows.Media.Ocr`):
//! nothing to install and nothing leaves the PC. Uses the English recognizer when it is
//! installed (MVP and map names are English), otherwise the user's profile languages.
use image::GrayImage;
use windows::{
    core::HSTRING,
    Globalization::Language,
    Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::{OcrEngine, OcrResult},
    Security::Cryptography::CryptographicBuffer,
    Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
};

pub struct Ocr {
    engine: OcrEngine,
    max_side: u32,
}

/// WinRT calls need the calling thread in an apartment; "already initialized" is fine
pub fn init_thread() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

impl Ocr {
    pub fn new() -> Result<Self, String> {
        let english = Language::CreateLanguage(&HSTRING::from("en-US"))
            .map_err(|e| format!("OCR language: {e}"))?;
        let engine = if OcrEngine::IsLanguageSupported(&english).unwrap_or(false) {
            OcrEngine::TryCreateFromLanguage(&english)
        } else {
            OcrEngine::TryCreateFromUserProfileLanguages()
        }
        .map_err(|e| format!("Windows OCR is not available: {e}"))?;
        let max_side = OcrEngine::MaxImageDimension().unwrap_or(2600);
        Ok(Self { engine, max_side })
    }

    /// Largest side the engine accepts; bigger images must be scaled down first
    pub fn max_side(&self) -> u32 {
        self.max_side
    }

    /// Text of a dark-on-white image, lines joined with spaces
    pub fn read(&self, image: &GrayImage) -> Result<String, String> {
        let result = self.recognize(image)?;
        Ok(result.Text().map(|t| t.to_string()).unwrap_or_default())
    }

    /// Each recognized line with the top edge of its first word, in image pixels
    pub fn read_lines(&self, image: &GrayImage) -> Result<Vec<Line>, String> {
        let result = self.recognize(image)?;
        let lines = result.Lines().map_err(|e| format!("OCR lines: {e}"))?;
        Ok(lines
            .into_iter()
            .filter_map(|line| {
                let text = line.Text().ok()?.to_string();
                let top = line.Words().ok()?.GetAt(0).ok()?.BoundingRect().ok()?.Y;
                Some(Line { text, top })
            })
            .collect())
    }

    fn recognize(&self, image: &GrayImage) -> Result<OcrResult, String> {
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 || width.max(height) > self.max_side {
            return Err(format!("OCR image size {width}x{height} not accepted"));
        }
        let bgra: Vec<u8> = image
            .pixels()
            .flat_map(|p| [p.0[0], p.0[0], p.0[0], 255])
            .collect();
        let buffer = CryptographicBuffer::CreateFromByteArray(&bgra)
            .map_err(|e| format!("OCR buffer: {e}"))?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
        )
        .map_err(|e| format!("OCR bitmap: {e}"))?;
        self.engine
            .RecognizeAsync(&bitmap)
            .and_then(|operation| operation.get())
            .map_err(|e| format!("OCR failed: {e}"))
    }
}

pub struct Line {
    pub text: String,
    pub top: f32,
}
