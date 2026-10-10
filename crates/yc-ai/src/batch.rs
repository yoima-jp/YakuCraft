use crate::TranslationEntry;

pub const MAX_BATCH_ENTRIES: usize = 50;
pub const MAX_BATCH_SOURCE_CHARACTERS: usize = 12_000;
pub const DEFAULT_CONCURRENCY: usize = 2;

pub(crate) fn split_batches(entries: &[TranslationEntry]) -> Vec<std::ops::Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut count = 0usize;
    let mut source_characters = 0usize;

    for (index, entry) in entries.iter().enumerate() {
        let entry_characters = entry.source.chars().count();
        let would_exceed_count = count == MAX_BATCH_ENTRIES;
        let would_exceed_characters = count > 0
            && source_characters.saturating_add(entry_characters) > MAX_BATCH_SOURCE_CHARACTERS;
        if would_exceed_count || would_exceed_characters {
            batches.push(start..index);
            start = index;
            count = 0;
            source_characters = 0;
        }

        source_characters = source_characters.saturating_add(entry_characters);
        count += 1;
    }

    if count > 0 {
        batches.push(start..entries.len());
    }
    batches
}
