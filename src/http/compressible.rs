//! Which of the embedded web assets are stored gzipped. `build.rs` compresses
//! them (this file is compiled into it too) and the server decides on the same
//! rule how to read them back.

/// True for text assets, which compress to a third or less; images and fonts
/// are compressed already.
pub fn is_compressible(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        [
            "html",
            "js",
            "mjs",
            "css",
            "svg",
            "json",
            "webmanifest",
            "txt",
        ]
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
    })
}
