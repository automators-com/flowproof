//! Windows' own OCR (`Windows.Media.Ocr`) as a vision engine.
//!
//! The engine Windows ships for reading screens - what the Snipping Tool's
//! text actions and PowerToys' Text Extractor use - trained on rendered UI
//! text rather than scanned pages. Against a live desktop it reads what
//! `ocrs` misses: thin glyphs alone on a button, small grey captions, the
//! text inside a tinted control. It reports true per-word boxes, so word
//! geometry needs no recovery from a detector.
//!
//! It is part of Windows 10 and 11 with no download; the recognition
//! languages are the user profile's, which installs English everywhere
//! flowproof has run. A machine without a usable OCR language falls back to
//! `ocrs` (see [`crate::vision::AutoOcr`]).

use image::RgbaImage;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine as WinOcrEngine;
use windows::Storage::Streams::DataWriter;

use flowproof_driver::{DriverError, PixelRect};

use crate::vision::{split_at_gaps, OcrEngine, OcrLine, OcrWord};

pub struct WindowsOcr {
    engine: WinOcrEngine,
    max_dimension: u32,
}

fn err(context: &str, e: windows::core::Error) -> DriverError {
    DriverError::Uia(format!("vision: Windows OCR {context}: {e}"))
}

impl WindowsOcr {
    /// The engine for the user's profile languages; an error when Windows
    /// has no OCR language installed for them.
    pub fn new() -> Result<Self, DriverError> {
        // WinRT needs the thread in a COM apartment. Already in one (UIA put
        // it there, or the host did) is fine either way.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
        }
        let engine = WinOcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|e| err("creating the engine", e))?;
        let max_dimension = WinOcrEngine::MaxImageDimension().unwrap_or(4096);
        Ok(Self {
            engine,
            max_dimension,
        })
    }

    fn read(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
        let (w, h) = frame.dimensions();
        if w == 0 || h == 0 {
            return Ok(Vec::new());
        }
        let mut bgra = frame.as_raw().clone();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        let writer = DataWriter::new().map_err(|e| err("buffer", e))?;
        writer.WriteBytes(&bgra).map_err(|e| err("buffer", e))?;
        let buffer = writer.DetachBuffer().map_err(|e| err("buffer", e))?;
        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            w as i32,
            h as i32,
            BitmapAlphaMode::Premultiplied,
        )
        .map_err(|e| err("bitmap", e))?;
        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .and_then(|op| op.join())
            .map_err(|e| err("recognizing", e))?;
        let mut lines = Vec::new();
        let read_lines = result.Lines().map_err(|e| err("lines", e))?;
        for i in 0..read_lines.Size().unwrap_or(0) {
            let Ok(line) = read_lines.GetAt(i) else {
                continue;
            };
            let Ok(read_words) = line.Words() else {
                continue;
            };
            let mut words = Vec::new();
            for j in 0..read_words.Size().unwrap_or(0) {
                let Ok(word) = read_words.GetAt(j) else {
                    continue;
                };
                let (Ok(text), Ok(r)) = (word.Text(), word.BoundingRect()) else {
                    continue;
                };
                let text = text.to_string_lossy();
                if text.trim().is_empty() {
                    continue;
                }
                words.push(OcrWord {
                    text,
                    rect: (
                        r.X.round() as i32,
                        r.Y.round() as i32,
                        r.Width.round().max(1.0) as u32,
                        r.Height.round().max(1.0) as u32,
                    ),
                });
            }
            // Windows groups every word on a row into one line, as ocrs
            // does; a keypad row or a toolbar is cut at its gaps the same way.
            let boxes: Vec<PixelRect> = words.iter().map(|w| w.rect).collect();
            for run in split_at_gaps(&boxes) {
                let words = words[run].to_vec();
                let text = words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let rect = words
                    .iter()
                    .skip(1)
                    .fold(words[0].rect, |acc, w| crate::vision::union(acc, w.rect));
                lines.push(OcrLine { text, rect, words });
            }
        }
        Ok(lines)
    }
}

impl OcrEngine for WindowsOcr {
    fn recognize(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
        let scale = std::env::var("FLOWPROOF_WINOCR_SCALE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        if scale > 1 {
            return self.recognize_closer(frame, scale);
        }
        self.read(frame)
    }

    /// Windows OCR reads glyphs from about 10px tall; a key label drawn
    /// thinner than that is found once enlarged, like ocrs.
    fn recognize_closer(
        &mut self,
        frame: &RgbaImage,
        scale: u32,
    ) -> Result<Vec<OcrLine>, DriverError> {
        let scale = scale.max(1);
        if frame.width().max(frame.height()) * scale > self.max_dimension {
            return self.read(frame);
        }
        let enlarged = image::imageops::resize(
            frame,
            frame.width() * scale,
            frame.height() * scale,
            image::imageops::FilterType::CatmullRom,
        );
        let s = scale as i32;
        let shrink = |(x, y, w, h): PixelRect| (x / s, y / s, w / scale, h / scale);
        let mut lines = self.read(&enlarged)?;
        for line in &mut lines {
            line.rect = shrink(line.rect);
            for word in &mut line.words {
                word.rect = shrink(word.rect);
            }
        }
        Ok(lines)
    }
}
