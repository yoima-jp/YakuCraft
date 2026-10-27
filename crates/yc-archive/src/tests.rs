// テストでは既定値から1項目だけ変えた ArchiveLimits を作る場面が多く、
// どの上限を変えたかが1行で読める代入形式の方が意図を追いやすいため許可する。
#![allow(clippy::field_reassign_with_default)]

use std::{
    fs,
    io::{Cursor, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::*;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl TemporaryFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

fn temporary_file(bytes: &[u8], extension: &str) -> TemporaryFile {
    let unique_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "yc-archive-test-{}-{unique_id}.{extension}",
        std::process::id()
    ));
    fs::write(&path, bytes).expect("write temporary fixture");
    TemporaryFile(path)
}

fn make_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, contents) in entries {
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer
            .start_file(path, options)
            .expect("start fixture entry");
        writer.write_all(contents).expect("write fixture entry");
    }
    writer.finish().expect("finish fixture JAR").into_inner()
}

fn patch_entry_name(bytes: &mut [u8], old_name: &[u8], new_name: &[u8]) {
    assert_eq!(old_name.len(), new_name.len());
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        if bytes[offset..offset + 4] == ZIP_LOCAL_FILE_HEADER {
            let name_size = u16::from_le_bytes([bytes[offset + 26], bytes[offset + 27]]) as usize;
            let extra_size = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
            let name_start = offset + 30;
            let name_end = name_start + name_size;
            if &bytes[name_start..name_end] == old_name {
                bytes[name_start..name_end].copy_from_slice(new_name);
            }
            let compressed_size =
                u32::from_le_bytes(bytes[offset + 18..offset + 22].try_into().unwrap()) as usize;
            offset = name_end + extra_size + compressed_size;
        } else if bytes[offset..offset + 4] == ZIP_CENTRAL_DIRECTORY_HEADER {
            let name_size = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
            let extra_size = u16::from_le_bytes([bytes[offset + 30], bytes[offset + 31]]) as usize;
            let comment_size =
                u16::from_le_bytes([bytes[offset + 32], bytes[offset + 33]]) as usize;
            let name_start = offset + 46;
            let name_end = name_start + name_size;
            if &bytes[name_start..name_end] == old_name {
                bytes[name_start..name_end].copy_from_slice(new_name);
            }
            offset = name_end + extra_size + comment_size;
        } else {
            offset += 1;
        }
    }
}

fn patch_uncompressed_size(bytes: &mut [u8], path: &[u8], forged_size: u32) {
    let forged_size = forged_size.to_le_bytes();
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        if bytes[offset..offset + 4] == ZIP_LOCAL_FILE_HEADER {
            let name_size = u16::from_le_bytes([bytes[offset + 26], bytes[offset + 27]]) as usize;
            let extra_size = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
            let name_start = offset + 30;
            let name_end = name_start + name_size;
            if &bytes[name_start..name_end] == path {
                bytes[offset + 22..offset + 26].copy_from_slice(&forged_size);
            }
            let compressed_size =
                u32::from_le_bytes(bytes[offset + 18..offset + 22].try_into().unwrap()) as usize;
            offset = name_end + extra_size + compressed_size;
        } else if bytes[offset..offset + 4] == ZIP_CENTRAL_DIRECTORY_HEADER {
            let name_size = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
            let extra_size = u16::from_le_bytes([bytes[offset + 30], bytes[offset + 31]]) as usize;
            let comment_size =
                u16::from_le_bytes([bytes[offset + 32], bytes[offset + 33]]) as usize;
            let name_start = offset + 46;
            let name_end = name_start + name_size;
            if &bytes[name_start..name_end] == path {
                bytes[offset + 24..offset + 28].copy_from_slice(&forged_size);
            }
            offset = name_end + extra_size + comment_size;
        } else {
            offset += 1;
        }
    }
}

#[test]
fn rejects_invalid_zip_and_wrong_extension() {
    let invalid = temporary_file(b"not a ZIP file", "jar");
    assert!(matches!(
        Archive::open(invalid.path()),
        Err(ArchiveError::InvalidJar(InvalidJarError::InvalidSignature))
    ));

    let valid_zip = make_jar(&[("a.txt", b"ok")]);
    let wrong_extension = temporary_file(&valid_zip, "zip");
    assert!(matches!(
        Archive::open(wrong_extension.path()),
        Err(ArchiveError::InvalidJar(
            InvalidJarError::InvalidExtension { .. }
        ))
    ));
}

#[test]
fn enforces_archive_size_and_entry_count_limits() {
    let bytes = make_jar(&[("a.txt", b"a"), ("b.txt", b"b")]);
    let file = temporary_file(&bytes, "jar");

    let mut limits = ArchiveLimits::default();
    limits.max_archive_size_bytes = 1;
    assert!(matches!(
        Archive::open_with_limits(file.path(), limits),
        Err(ArchiveError::InvalidJar(
            InvalidJarError::FileTooLarge { .. }
        ))
    ));

    let mut limits = ArchiveLimits::default();
    limits.max_entries = 1;
    assert!(matches!(
        Archive::open_with_limits(file.path(), limits),
        Err(ArchiveError::InvalidJar(InvalidJarError::TooManyEntries {
            count: 2,
            limit: 1
        }))
    ));
}

#[test]
fn excludes_declared_oversized_entry() {
    let bytes = make_jar(&[("large.txt", &[b'x'; 32])]);
    let file = temporary_file(&bytes, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_text_size_bytes = 8;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();

    assert!(matches!(
        archive.read_entry("large.txt", EntryKind::Text),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::DeclaredSizeExceeded { .. },
            ..
        }))
    ));
    assert_eq!(archive.cumulative_read_bytes().unwrap(), 0);
}

#[test]
fn enforces_actual_size_when_header_understates_entry() {
    let contents = vec![b'x'; 256 * 1024];
    let mut bytes = make_jar(&[("forged.txt", &contents)]);
    patch_uncompressed_size(&mut bytes, b"forged.txt", 1);
    let file = temporary_file(&bytes, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_text_size_bytes = 64;
    limits.max_compression_ratio = u32::MAX;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();

    let result = archive.read_entry("forged.txt", EntryKind::Text);
    assert!(matches!(
        result,
        Err(ArchiveError::ArchiveRead(ArchiveReadError::ReadEntry {
            source,
            ..
        })) if source.kind() == std::io::ErrorKind::InvalidData
    ));
    assert!(archive.cumulative_read_bytes().unwrap() <= 65);
}

#[test]
fn detects_duplicate_and_unsafe_paths() {
    let mut duplicate_jar = make_jar(&[("a.txt", b"first"), ("b.txt", b"second")]);
    patch_entry_name(&mut duplicate_jar, b"b.txt", b"a.txt");
    let duplicate_file = temporary_file(&duplicate_jar, "jar");
    let mut archive = Archive::open(duplicate_file.path()).unwrap();
    assert_eq!(archive.list_entries().len(), 2);
    assert!(archive.list_entries().iter().all(|entry| {
        entry
            .exclusion_reasons
            .contains(&EntryExclusionReason::DuplicatePath)
    }));
    assert!(matches!(
        archive.read_entry("a.txt", EntryKind::Text),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::DuplicatePath,
            ..
        }))
    ));

    let traversal_jar = make_jar(&[("../outside.txt", b"unsafe")]);
    let traversal_file = temporary_file(&traversal_jar, "jar");
    let mut traversal_archive = Archive::open(traversal_file.path()).unwrap();
    assert!(matches!(
        traversal_archive.list_entries()[0].exclusion_reasons[0],
        EntryExclusionReason::UnsafePath(PathSafetyError::ParentTraversal)
    ));
    assert!(matches!(
        traversal_archive.read_entry("../outside.txt", EntryKind::Text),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::UnsafePath(PathSafetyError::ParentTraversal),
            ..
        }))
    ));
    assert_eq!(
        normalize_archive_path("C:/outside.txt"),
        Err(PathSafetyError::DrivePrefix)
    );
    assert_eq!(
        normalize_archive_path("a\\b.txt"),
        Err(PathSafetyError::Backslash)
    );
}

#[test]
fn flags_entries_with_abnormal_compression_ratio() {
    let bytes = make_jar(&[("repetitive.txt", &[b'a'; 4096])]);
    let file = temporary_file(&bytes, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_compression_ratio = 2;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();

    assert!(
        archive.list_entries()[0]
            .exclusion_reasons
            .iter()
            .any(|reason| matches!(
                reason,
                EntryExclusionReason::CompressionRatioExceeded { .. }
            ))
    );
    assert!(matches!(
        archive.read_entry("repetitive.txt", EntryKind::Text),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::CompressionRatioExceeded { .. },
            ..
        }))
    ));
}

#[test]
fn detects_signature_files_at_meta_inf_root() {
    let bytes = make_jar(&[
        ("META-INF/TEST.SF", b"signature"),
        ("META-INF/signer.RSA", b"signature"),
        ("META-INF/sub/ignored.DSA", b"not a root signature file"),
    ]);
    let file = temporary_file(&bytes, "jar");
    let archive = Archive::open(file.path()).unwrap();

    assert!(archive.is_signed());
    assert_eq!(
        archive.signature_files(),
        vec!["META-INF/TEST.SF", "META-INF/signer.RSA"]
    );
}

#[test]
fn discovers_conventional_and_metadata_nested_jars_and_limits_depth() {
    let deepest = make_jar(&[("payload.txt", b"deep")]);
    let inner = make_jar(&[("META-INF/jars/deeper.jar", &deepest)]);
    let root = make_jar(&[
        ("META-INF/jars/lib.jar", &inner),
        ("META-INF/jarjar/second.jar", &deepest),
        ("vendor/metadata-entry.bin", &inner),
    ]);
    let file = temporary_file(&root, "jar");
    let mut archive = Archive::open(file.path()).unwrap();

    let mut nested = archive
        .discover_nested(&["vendor/metadata-entry.bin".to_owned()])
        .unwrap();
    assert_eq!(nested.len(), 3);
    assert!(nested.iter().all(|item| item.archive.is_some()));
    let inner_archive = nested
        .iter_mut()
        .find(|item| item.path == "META-INF/jars/lib.jar")
        .unwrap()
        .archive
        .as_mut()
        .unwrap();
    assert!(matches!(
        inner_archive.discover_nested(&[]),
        Err(ArchiveError::NestedDepthExceeded)
    ));
}

#[test]
fn skips_nested_jar_over_configured_limit() {
    let inner = make_jar(&[("payload.txt", &[b'x'; 128])]);
    let root = make_jar(&[("META-INF/jars/lib.jar", &inner)]);
    let file = temporary_file(&root, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_nested_jar_size_bytes = 32;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();

    let nested = archive.discover_nested(&[]).unwrap();
    assert_eq!(nested.len(), 1);
    assert!(nested[0].archive.is_none());
    assert!(matches!(
        nested[0].skipped_reason,
        Some(NestedJarSkippedReason::EntryExcluded(
            EntryExclusionReason::DeclaredSizeExceeded { .. }
        ))
    ));
}

#[test]
fn nested_reads_share_the_root_cumulative_budget() {
    let inner = make_jar(&[("payload.txt", b"abc")]);
    let root = make_jar(&[("META-INF/jars/lib.jar", &inner)]);
    let file = temporary_file(&root, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_cumulative_read_bytes = inner.len() as u64;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();

    let mut nested = archive.discover_nested(&[]).unwrap();
    assert_eq!(archive.cumulative_read_bytes().unwrap(), inner.len() as u64);
    let nested_archive = nested[0].archive.as_mut().unwrap();
    assert!(matches!(
        nested_archive.read_entry("payload.txt", EntryKind::Text),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::CumulativeReadLimitExceeded { .. },
            ..
        }))
    ));
}

#[test]
fn cancels_nested_discovery_while_reading_nested_jar_bytes() {
    let mut state = 0x1234_5678_u32;
    let contents: Vec<u8> = (0..512 * 1024)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect();
    let inner = make_jar(&[("payload.bin", &contents)]);
    let root = make_jar(&[("META-INF/jars/lib.jar", &inner)]);
    let file = temporary_file(&root, "jar");
    let mut archive = Archive::open(file.path()).unwrap();
    let mut checks = 0;

    let result = archive.discover_nested_with_cancellation(&[], || {
        checks += 1;
        checks >= 8
    });

    assert!(matches!(result, Err(NestedDiscoveryError::Cancelled)));
    assert!(archive.cumulative_read_bytes().unwrap() < inner.len() as u64);
}

#[test]
fn independent_reads_do_not_exhaust_the_analysis_budget() {
    let bytes = make_jar(&[("payload.txt", b"abc")]);
    let file = temporary_file(&bytes, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_cumulative_read_bytes = 3;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();

    for _ in 0..4 {
        assert_eq!(
            archive
                .read_entry_independent("payload.txt", EntryKind::Text)
                .unwrap(),
            b"abc"
        );
    }
    assert_eq!(archive.cumulative_read_bytes().unwrap(), 0);
    archive.read_entry("payload.txt", EntryKind::Text).unwrap();
    assert!(archive.read_entry("payload.txt", EntryKind::Text).is_err());
    assert_eq!(
        archive
            .read_entry_independent("payload.txt", EntryKind::Text)
            .unwrap(),
        b"abc"
    );
    // 独立読込が成功しても、すでに超過した解析の予算は復活させない。
    assert!(archive.read_entry("payload.txt", EntryKind::Text).is_err());
}

#[test]
fn independent_nested_reads_preserve_the_shared_analysis_budget() {
    let inner = make_jar(&[("payload.txt", b"abc")]);
    let root = make_jar(&[("META-INF/jars/lib.jar", &inner)]);
    let file = temporary_file(&root, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_cumulative_read_bytes = inner.len() as u64;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();
    let mut nested = archive.discover_nested(&[]).unwrap();
    let nested_archive = nested[0].archive.as_mut().unwrap();

    assert_eq!(
        nested_archive
            .read_entry_independent("payload.txt", EntryKind::Text)
            .unwrap(),
        b"abc"
    );
    assert_eq!(archive.cumulative_read_bytes().unwrap(), inner.len() as u64);
    assert!(matches!(
        nested_archive.read_entry("payload.txt", EntryKind::Text),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::CumulativeReadLimitExceeded { .. },
            ..
        }))
    ));
}

#[test]
fn independent_reads_enforce_size_limits_and_restore_budget_after_errors() {
    let mut bytes = make_jar(&[("forged.txt", &[b'x'; 4096]), ("valid.txt", b"ok")]);
    patch_uncompressed_size(&mut bytes, b"forged.txt", 1);
    let file = temporary_file(&bytes, "jar");
    let mut limits = ArchiveLimits::default();
    limits.max_text_size_bytes = 64;
    limits.max_compression_ratio = u32::MAX;
    let mut archive = Archive::open_with_limits(file.path(), limits).unwrap();
    archive.read_entry("valid.txt", EntryKind::Text).unwrap();

    let result = archive.read_entry_independent("forged.txt", EntryKind::Text);
    assert!(matches!(
        result,
        Err(ArchiveError::ArchiveRead(ArchiveReadError::ReadEntry {
            source,
            ..
        })) if source.kind() == std::io::ErrorKind::InvalidData
    ));
    assert_eq!(archive.cumulative_read_bytes().unwrap(), 2);
    assert!(matches!(
        archive.read_entry_independent("valid.txt", EntryKind::Custom(1)),
        Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded {
            reason: EntryExclusionReason::DeclaredSizeExceeded { .. },
            ..
        }))
    ));
    assert_eq!(archive.cumulative_read_bytes().unwrap(), 2);
}

#[test]
fn computes_known_sha256_without_loading_the_file_as_a_single_buffer() {
    let file = temporary_file(b"abc", "bin");
    let digest = sha256_file(file.path()).unwrap();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        hex,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
