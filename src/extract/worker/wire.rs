//! The messages between the server and `paperjam extract-worker`.
//!
//! The server writes one request to the worker's stdin and closes it:
//!
//! ```text
//! u32 LE header length | header (JSON, a `Header`) | file bytes to end of input
//! ```
//!
//! The worker answers on stdout with frames, and exits when it is done:
//!
//! ```text
//! u8 tag | u32 LE payload length | payload
//! ```
//!
//! | tag | payload |
//! |-----|---------|
//! | `T` | JSON `Vec<PageText>`: the embedded text of every page of a PDF |
//! | `P` | a rendered page: `u32 n, u32 width, u32 height, u64 micros`, then RGB8 |
//! | `Q` | a page that did not render: `u32 n`, then JSON `WireError` |
//! | `I` | a decoded image: `u32 width, u32 height`, then RGB8 |
//! | `E` | JSON `WireError`: the request failed |
//! | `D` | nothing: all pages were sent |
//!
//! The worker handles hostile files, so the server trusts nothing it reads
//! back: lengths are bounded and a pixel buffer must match its size.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{ErrorKind, ServiceError};

/// The largest frame the server accepts: a render of 100 megapixels.
pub const MAX_FRAME: usize = 320 * 1024 * 1024;
/// The largest request header the worker accepts.
pub const MAX_HEADER: usize = 64 * 1024;

pub const TEXT: u8 = b'T';
pub const PAGE: u8 = b'P';
pub const PAGE_FAILED: u8 = b'Q';
pub const IMAGE: u8 = b'I';
pub const FAILED: u8 = b'E';
pub const DONE: u8 = b'D';

#[derive(Debug, Serialize, Deserialize)]
pub struct Header {
    pub request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Embedded text of every page, then every page rendered. Pages that
    /// need reading are rendered at `ocr_dpi`, the others at `preview_dpi`.
    Pdf { ocr_dpi: u32, preview_dpi: u32 },
    /// An upright, RGB image scaled down to `max_edge` along its longest side.
    Image { mime: String, max_edge: u32 },
}

/// What a PDF says on one page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageText {
    /// The embedded text, trimmed; `None` for a page without any.
    pub text: Option<String>,
    pub score: f32,
    /// Pixels per point of the scan the page is made of, when it is one.
    pub density: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WireError {
    pub kind: String,
    pub code: String,
    pub message: String,
}

/// Every code the worker reports. A code is `&'static str`, so one read off
/// the wire is matched against this list; anything else is not trusted.
const CODES: &[&str] = &[
    "pdf_encrypted",
    "pdf_corrupt",
    "too_many_pages",
    "page_out_of_range",
    "image_corrupt",
    "image_too_large",
    "heic_unsupported",
];

fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Internal => "internal",
        _ => "invalid",
    }
}

impl From<&ServiceError> for WireError {
    fn from(e: &ServiceError) -> Self {
        WireError {
            kind: kind_name(e.kind).to_string(),
            code: e.code.to_string(),
            message: e.message.clone(),
        }
    }
}

impl WireError {
    /// The error the worker reported; `fallback` when its code is not one
    /// this build knows.
    pub fn into_service(self, fallback: &'static str) -> ServiceError {
        let Some(code) = CODES.iter().find(|c| **c == self.code) else {
            return ServiceError::invalid(fallback, self.message);
        };
        let kind = if self.kind == "internal" {
            ErrorKind::Internal
        } else {
            ErrorKind::Invalid
        };
        ServiceError::new(kind, code, self.message)
    }
}

/// Writes one frame whose payload is `parts` end to end.
pub fn write_frame(out: &mut impl Write, tag: u8, parts: &[&[u8]]) -> io::Result<()> {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let len = u32::try_from(len).map_err(|_| io::Error::other("frame too large"))?;
    out.write_all(&[tag])?;
    out.write_all(&len.to_le_bytes())?;
    for part in parts {
        out.write_all(part)?;
    }
    out.flush()
}

/// Reads one frame; `None` when the stream ends between frames.
pub async fn read_frame(from: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match from.read_exact(&mut head[..1]).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    from.read_exact(&mut head[1..]).await?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame larger than allowed",
        ));
    }
    let mut payload = vec![0u8; len];
    from.read_exact(&mut payload).await?;
    Ok(Some((head[0], payload)))
}

/// Splits `n` little-endian `u32`s off the front of a payload.
pub fn take_u32s<const N: usize>(payload: &[u8]) -> Option<([u32; N], &[u8])> {
    let (head, rest) = payload.split_at_checked(N * 4)?;
    let mut out = [0u32; N];
    let (words, _) = head.as_chunks::<4>();
    for (slot, bytes) in out.iter_mut().zip(words) {
        *slot = u32::from_le_bytes(*bytes);
    }
    Some((out, rest))
}

/// An RGB8 buffer of `width` × `height` pixels, if `bytes` is exactly one.
pub fn rgb_image(width: u32, height: u32, bytes: &[u8]) -> Option<::image::RgbImage> {
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(3)?;
    if width == 0 || height == 0 || bytes.len() != expected {
        return None;
    }
    ::image::RgbImage::from_raw(width, height, bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_and_end_cleanly() {
        let mut wire = Vec::new();
        write_frame(&mut wire, PAGE, &[&7u32.to_le_bytes(), b"abc"]).unwrap();
        write_frame(&mut wire, DONE, &[]).unwrap();
        let mut reader = wire.as_slice();
        let (tag, payload) = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(tag, PAGE);
        let ([n], rest) = take_u32s::<1>(&payload).unwrap();
        assert_eq!((n, rest), (7, &b"abc"[..]));
        assert_eq!(read_frame(&mut reader).await.unwrap().unwrap().0, DONE);
        assert!(read_frame(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_cut_frame_and_an_oversized_one_are_errors() {
        let mut cut = Vec::new();
        write_frame(&mut cut, TEXT, &[b"0123456789"]).unwrap();
        cut.truncate(cut.len() - 3);
        assert!(read_frame(&mut cut.as_slice()).await.is_err());

        let mut huge = vec![TEXT];
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut huge.as_slice()).await.is_err());
    }

    #[test]
    fn pixels_must_match_their_size() {
        assert!(rgb_image(2, 2, &[0; 12]).is_some());
        assert!(rgb_image(2, 2, &[0; 11]).is_none());
        assert!(rgb_image(0, 2, &[]).is_none());
        assert!(rgb_image(u32::MAX, u32::MAX, &[0; 12]).is_none());
    }

    #[test]
    fn only_known_codes_survive_the_wire() {
        let known = WireError {
            kind: "invalid".into(),
            code: "pdf_encrypted".into(),
            message: "locked".into(),
        };
        assert_eq!(known.into_service("pdf_corrupt").code, "pdf_encrypted");
        let unknown = WireError {
            kind: "invalid".into(),
            code: "made_up".into(),
            message: "x".into(),
        };
        assert_eq!(unknown.into_service("pdf_corrupt").code, "pdf_corrupt");
    }
}
