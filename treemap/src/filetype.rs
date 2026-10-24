//! File types from file names: the extension, a fixed category per extension and its tile
//! colour, and per-extension totals for the footer. Pure functions of the name, so a tile's
//! colour is known when it is drawn, before any totals exist.

use crate::Theme;
use scan::{NodeId, Tree};
use std::collections::HashMap;

/// The extension of `name`: 1 to 12 ASCII letters or digits after the last `.`, in the case
/// of the name. `None` for no dot, a name whose only dot is its first character (".gitignore"),
/// or anything else after the last dot (empty, too long, a space, a dash, non-ASCII).
pub fn extension(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    let ext = &name[dot + 1..];
    (dot > 0 && (1..=12).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(ext)
}

/// A fixed group of file types. Each has one tile colour per theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Video,
    Image,
    Audio,
    Archive,
    Program,
    Document,
    Code,
    DiskImage,
    GameData,
    Data,
    Other,
}

impl Category {
    /// Every category, `Other` last.
    pub const ALL: [Category; 11] = [
        Category::Video,
        Category::Image,
        Category::Audio,
        Category::Archive,
        Category::Program,
        Category::Document,
        Category::Code,
        Category::DiskImage,
        Category::GameData,
        Category::Data,
        Category::Other,
    ];
}

/// The category of an extension in any case, as `extension` gives it; `Other` if unknown.
pub fn category_of_ext(ext: &str) -> Category {
    use Category::*;
    // Lower case on the stack: this runs for every tile drawn.
    let mut buf = [0u8; 12];
    let Some(low) = buf.get_mut(..ext.len()) else { return Other };
    low.copy_from_slice(ext.as_bytes());
    low.make_ascii_lowercase();
    match std::str::from_utf8(low).unwrap_or("") {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "mpg" | "mpeg" | "flv" | "m2ts" | "mts" | "vob"
        | "3gp" | "bik" | "usm" => Video,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "heic" | "heif" | "avif" | "raw"
        | "cr2" | "cr3" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" | "tga" | "dds" | "exr" | "hdr" | "ktx" => {
            Image
        }
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "opus" | "m4a" | "wma" | "aiff" | "mid" | "midi" | "wem"
        | "bnk" => Audio,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz4" | "lzma" | "cab" | "jar"
        | "nupkg" | "whl" | "crate" | "apk" | "zipx" => Archive,
        "exe" | "dll" | "sys" | "msi" | "msp" | "msix" | "appx" | "efi" | "drv" | "ocx" | "cpl" | "mui"
        | "com" | "scr" | "so" | "dylib" | "node" | "winmd" | "pyd" | "lib" | "a" | "o" | "obj" | "rlib"
        | "rmeta" | "pdb" | "pch" | "ilk" => Program,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "rtf" | "txt"
        | "md" | "epub" | "one" | "chm" => Document,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "java" | "kt" | "py" | "js" | "mjs" | "cjs" | "ts"
        | "tsx" | "jsx" | "go" | "rb" | "php" | "swift" | "lua" | "sh" | "ps1" | "bat" | "cmd" | "html" | "htm"
        | "css" | "scss" | "vue" | "sql" | "slint" | "map" => Code,
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "qcow2" | "vdi" | "wim" | "esd" | "swm" | "dmg" => DiskImage,
        "pak" | "ucas" | "utoc" | "uasset" | "uexp" | "ubulk" | "umap" | "ba2" | "bsa" | "esm" | "esp" | "vpk"
        | "forge" | "bundle" | "assets" | "resource" | "unity3d" | "rpf" | "pck" | "wad" | "minizip" => GameData,
        "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "dat" | "bin" | "json" | "xml" | "csv" | "log" | "yaml"
        | "yml" | "toml" | "ini" | "cfg" | "jsonl" | "parquet" | "ldb" | "edb" | "etl" | "evtx" | "blob" | "idx"
        | "pack" | "cache" | "safetensors" | "gguf" | "ckpt" | "onnx" | "pt" | "pth" => Data,
        _ => Other,
    }
}

/// The category of the file `name`: from its extension, `Other` when it has none.
pub fn category_of(name: &str) -> Category {
    extension(name).map_or(Category::Other, category_of_ext)
}

/// The tile colour of category `c` in `theme`, RGB.
pub fn colour(c: Category, theme: Theme) -> [u8; 3] {
    use Category::*;
    // Dark: the mockup palette (layout-v2.html, style.html). Light: the same hues darkened until
    // they have 3:1 contrast with the light backdrop #f3f3f3; Video, Image and Audio already do.
    let hex: u32 = match (theme, c) {
        (Theme::Dark, Program) => 0x3b8fe6,
        (Theme::Dark, Archive) => 0xe8872b,
        (Theme::Dark, Data) => 0xc9c42a,
        (Theme::Dark, GameData) => 0x2fd6b8,
        (Theme::Dark, Video) => 0xd94b8c,
        (Theme::Dark, Image) => 0x9b5de0,
        (Theme::Dark, Code) => 0x8dbb3a,
        (Theme::Dark, Document) => 0xa3a8f0,
        (Theme::Dark, Audio) => 0xe5533a,
        (Theme::Dark, DiskImage) => 0x4aa3b0,
        (Theme::Dark, Other) => 0x6e6e6e,
        (Theme::Light, Program) => 0x3888db,
        (Theme::Light, Archive) => 0xc37124,
        (Theme::Light, Data) => 0x8d891d,
        (Theme::Light, GameData) => 0x20947f,
        (Theme::Light, Video) => 0xd94b8c,
        (Theme::Light, Image) => 0x9b5de0,
        (Theme::Light, Code) => 0x6d902d,
        (Theme::Light, Document) => 0x7e81b9,
        (Theme::Light, Audio) => 0xe5533a,
        (Theme::Light, DiskImage) => 0x418f9b,
        (Theme::Light, Other) => 0x8a8a8a,
    };
    let [_, r, g, b] = hex.to_be_bytes();
    [r, g, b]
}

/// Bytes and file count of one extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTotal {
    /// Lower case; "" for files with no extension.
    pub ext: String,
    pub bytes: u64,
    pub files: u64,
}

/// Totals per lower-case extension of the files under `root`, largest first (bytes desc, then
/// ext asc). Folders are not counted; files with no size count with 0 bytes.
pub fn type_totals(tree: &Tree, root: NodeId) -> Vec<TypeTotal> {
    // Key: the lower-case extension packed into a u128 (at most 12 ASCII bytes, none of them 0;
    // 0 = no extension), so the pass allocates nothing per file.
    let mut sums: HashMap<u128, (u64, u64)> = HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let node = tree.node(n);
        if node.is_dir {
            stack.extend(tree.children(n));
            continue;
        }
        let key = extension(tree.name(n))
            .map_or(0, |e| e.bytes().fold(0u128, |k, b| (k << 8) | u128::from(b.to_ascii_lowercase())));
        let sum = sums.entry(key).or_default();
        sum.0 += node.size;
        sum.1 += 1;
    }
    let mut out: Vec<TypeTotal> = sums
        .into_iter()
        .map(|(key, (bytes, files))| {
            let ext = key.to_be_bytes().iter().filter(|&&b| b != 0).map(|&b| char::from(b)).collect();
            TypeTotal { ext, bytes, files }
        })
        .collect();
    out.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.ext.cmp(&b.ext)));
    out
}
