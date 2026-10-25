mod error;
mod ftb;
mod kubejs;
mod model;
mod scan;
mod snbt;

pub use error::ModpackError;
pub use model::{
    FtbQuestsStatus, ModpackData, ModpackEntry, ModpackFile, ModpackFileKind, ModpackScan,
    OutputFile,
};
pub use scan::{analyze_files, resolve_game_directory, scan_instance};
pub use snbt::{SnbtDocument, SnbtValue, parse_snbt, write_snbt};

use std::collections::BTreeMap;

/// Translation overrides are keyed by `ModpackEntry::key`. Missing overrides use
/// an existing ja_jp value when present, then fall back to English.
pub fn write_ja_jp(
    data: &ModpackData,
    translations: &BTreeMap<String, String>,
) -> Result<Vec<OutputFile>, ModpackError> {
    ftb::write_ftb_outputs(data, translations).and_then(|mut files| {
        files.extend(kubejs::write_kubejs_outputs(data, translations)?);
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    })
}

#[cfg(test)]
mod tests;
