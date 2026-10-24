//! File types from file names: the extension, a fixed category per extension and its tile
//! colour, and per-extension totals for the footer. Pure functions of the name, so a tile's
//! colour is known when it is drawn, before any totals exist.

use crate::Theme;
use scan::{NodeId, Tree};

/// The extension of `name`: 1 to 12 ASCII letters or digits after the last `.`, in the case
/// of the name. `None` for no dot, a name whose only dot is its first character (".gitignore"),
/// or anything else after the last dot (empty, too long, a space, a dash, non-ASCII).
pub fn extension(name: &str) -> Option<&str> {
    let _ = name;
    todo!()
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
    let _ = ext;
    todo!()
}

/// The category of the file `name`: from its extension, `Other` when it has none.
pub fn category_of(name: &str) -> Category {
    let _ = name;
    todo!()
}

/// The tile colour of category `c` in `theme`, RGB.
pub fn colour(c: Category, theme: Theme) -> [u8; 3] {
    let _ = (c, theme);
    todo!()
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
    let _ = (tree, root);
    Vec::new()
}
