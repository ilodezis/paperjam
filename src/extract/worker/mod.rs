//! Parsing of untrusted files in a child process.
//!
//! PDF and image parsers are large, young or written in C, and they read
//! files that third parties send. A hang, a memory bomb or a crash in one of
//! them must end a single child, `paperjam extract-worker`, and not the
//! server and the uploads and requests it is serving. [`Isolation`] runs the
//! child and speaks the [`wire`] protocol to it; [`child::run`] is the child.

pub mod child;
mod parent;
pub mod wire;

pub use parent::{Isolation, PdfRun, Rendered};
