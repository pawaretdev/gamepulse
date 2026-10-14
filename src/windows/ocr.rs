//! Text recognition with the OCR engine built into Windows (`Windows.Media.Ocr`):
//! nothing to install and nothing leaves the PC. Uses the English recognizer when it is
//! installed (MVP and map names are English), otherwise the user's profile languages.
use image::GrayImage;
use windows::{
    core::HSTRING,
    Globalization::Language,
    Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
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
        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .and_then(|operation| operation.get())
            .map_err(|e| format!("OCR failed: {e}"))?;
        Ok(result.Text().map(|t| t.to_string()).unwrap_or_default())
    }
}
