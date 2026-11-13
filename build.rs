//! Build steps: re-embed migrations and the web build when they change, and
//! prepare the web build for embedding with its text assets gzipped: the
//! assets go under `$OUT_DIR/web` and a table of them, for `include_bytes!`,
//! into `$OUT_DIR/web_assets.rs`.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;

#[path = "src/http/compressible.rs"]
mod compressible;

fn main() {
    // Re-embed migrations whenever a migration file changes.
    println!("cargo:rerun-if-changed=src/db/migrations");
    // Re-embed web assets whenever the frontend build changes.
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=src/http/compressible.rs");

    // `web/dist` is optional (the Rust gate does not need Node): without it
    // the embedded folder is empty and the server shows a placeholder page.
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let out = out_dir.join("web");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("create the embedded web folder");
    let mut names = Vec::new();
    let dist = Path::new("web/dist");
    if dist.is_dir() {
        embed(dist, dist, &out, &mut names);
    }
    // Sorted, so the server can binary-search it.
    names.sort();
    let mut table = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for name in &names {
        let file = out.join(name);
        table.push_str(&format!(
            "    ({name:?}, include_bytes!({:?})),\n",
            file.to_string_lossy()
        ));
    }
    table.push_str("];\n");
    fs::write(out_dir.join("web_assets.rs"), table).expect("write the web asset table");
}

/// Copies the files under `dir` into `out`, keeping their paths, and lists
/// their names in `names`; text assets go in gzipped under the same name.
fn embed(root: &Path, dir: &Path, out: &Path, names: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read the web build") {
        let path = entry.expect("read the web build").path();
        if path.is_dir() {
            embed(root, &path, out, names);
            continue;
        }
        let relative = path.strip_prefix(root).expect("inside the web build");
        let name = relative.to_string_lossy().replace('\\', "/");
        let target = out.join(relative);
        names.push(name.clone());
        fs::create_dir_all(target.parent().expect("has a parent")).expect("create a folder");
        let bytes = fs::read(&path).expect("read a web asset");
        if compressible::is_compressible(&name) {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
            encoder.write_all(&bytes).expect("compress a web asset");
            fs::write(&target, encoder.finish().expect("compress a web asset"))
        } else {
            fs::write(&target, bytes)
        }
        .expect("write an embedded web asset");
    }
}
