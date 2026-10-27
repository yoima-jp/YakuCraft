use std::{
    fs,
    io::{Cursor, Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use yc_archive::sha256_file;
use yc_language::{
    DiscoveredLanguageFile, LanguageFileFormat, TranslationEntry, TranslationOrigin, WriterOptions,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    ExportError, ExportInput, ExportKind, ExportValidationRequest, LanguageFileArtifact,
    ManualPackFormat, MergedPackMod, MergedResourcePackOptions, NamespaceExport,
    PlaceholderWarning, ResourcePackMod, ResourcePackOptions, ValidationIssueCode,
    ValidationSeverity, create_language_files, export_merged_resource_pack, export_modified_jar,
    export_resource_pack, install_resource_pack, install_resource_pack_to,
    instance_dir_from_mods_dir, looks_like_instance_dir, merged_resource_pack_filename,
    modified_jar_filename, pack_format_for_version, replace_jar_in_place, resource_pack_filename,
    restore_jar_from_backup, validate_export, write_language_files,
};

static TEMP_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

fn translation(namespace: &str, key: &str, text: &str, sort_index: usize) -> TranslationEntry {
    TranslationEntry {
        namespace: namespace.to_owned(),
        key: key.to_owned(),
        source_text: Some(format!("source.{key}")),
        source_locale: Some("en_us".to_owned()),
        target_text: text.to_owned(),
        target_original_text: None,
        translated_source_text: None,
        origin: TranslationOrigin::Edited,
        flags: Vec::new(),
        sort_index,
    }
}

fn discovered(
    namespace: &str,
    locale: &str,
    format: LanguageFileFormat,
    container_path: &str,
    file_name: &str,
) -> DiscoveredLanguageFile {
    DiscoveredLanguageFile {
        namespace: namespace.to_owned(),
        locale: locale.to_owned(),
        normalized_locale: locale.to_ascii_lowercase(),
        format,
        container_path: container_path.to_owned(),
        archive_path: format!("assets/{namespace}/lang/{file_name}"),
        file_name: file_name.to_owned(),
    }
}

fn input<'a>(
    entries: &'a [TranslationEntry],
    namespaces: &'a [NamespaceExport],
    language_files: &'a [DiscoveredLanguageFile],
    malformed: &'a [String],
    placeholders: &'a [PlaceholderWarning],
    source_locale: &'a str,
) -> ExportInput<'a> {
    ExportInput {
        entries,
        namespaces,
        language_files,
        legacy_parse_escapes_namespaces: &[],
        source_locale,
        writer_options: WriterOptions::default(),
        malformed_translation_files: malformed,
        placeholder_warnings: placeholders,
        patchouli_documents: &[],
    }
}

fn temporary_directory() -> PathBuf {
    let count = TEMP_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("yc-export-test-{}-{count}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn zip_bytes(entries: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content, method) in entries {
        writer
            .start_file(
                *name,
                SimpleFileOptions::default().compression_method(*method),
            )
            .unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn read_entry(path: &std::path::Path, name: &str) -> Vec<u8> {
    let file = fs::File::open(path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).unwrap();
    bytes
}

fn read_raw_entry(path: &std::path::Path, name: &str) -> (Vec<u8>, CompressionMethod) {
    let file = fs::File::open(path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    let index = (0..archive.len())
        .find(|index| {
            archive
                .name_for_index(*index)
                .is_some_and(|entry_name| entry_name.is_ok_and(|entry_name| entry_name == name))
        })
        .unwrap();
    let mut entry = archive.by_index_raw(index).unwrap();
    let compression = entry.compression();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).unwrap();
    (bytes, compression)
}

#[test]
fn language_file_json_and_legacy_output_are_deterministic() {
    let entries = [
        translation("mod", "z.last", "最後", 2),
        translation("mod", "a.first", "最初", 1),
    ];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let json_files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let json_input = input(
        &entries,
        &namespaces,
        &json_files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    let first = create_language_files(&json_input).unwrap();
    let second = create_language_files(&json_input).unwrap();
    assert_eq!(first, second);
    assert_eq!(first[0].file_name, "ja_jp.json");
    assert_eq!(
        first[0].bytes,
        "{\n  \"a.first\": \"最初\",\n  \"z.last\": \"最後\"\n}\n".as_bytes()
    );

    let legacy_files = [discovered(
        "mod",
        "en_US",
        LanguageFileFormat::LegacyLang,
        "",
        "en_US.lang",
    )];
    let legacy_input = input(
        &entries,
        &namespaces,
        &legacy_files,
        &empty,
        &no_placeholders,
        "en_us",
    );
    let legacy = create_language_files(&legacy_input).unwrap();
    assert_eq!(legacy[0].file_name, "ja_JP.lang");
    assert_eq!(legacy[0].bytes, "a.first=最初\nz.last=最後\n".as_bytes());
}

#[test]
fn resource_pack_language_paths_and_filename_follow_version_boundaries() {
    let directory = temporary_directory();
    let entries = [
        translation("mod", "key", "訳", 0),
        translation("mod", "empty", "", 1),
        translation("mod", "whitespace", "　 \t", 2),
    ];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    let mods = [ResourcePackMod {
        name: Some("mod".to_owned()),
        version: Some("2.0".to_owned()),
    }];
    for (version, expected_path, expected_file_name) in [
        ("1.10.2", "assets/mod/lang/ja_JP.lang", "mod-2.0-JP_EX.zip"),
        ("1.11", "assets/mod/lang/ja_jp.lang", "mod-2.0-JP_EX.zip"),
        ("1.13", "assets/mod/lang/ja_jp.json", "mod-2.0-JP_EX.zip"),
    ] {
        let output = export_resource_pack(
            &project,
            ResourcePackOptions {
                mods: &mods,
                source_jar_filename: "mod.jar",
                minecraft_version: Some(version),
                manual_format: None,
            },
            &directory,
            false,
        )
        .unwrap();
        assert_eq!(output.file_name().unwrap(), expected_file_name);
        assert_eq!(
            read_entry(&output, expected_path),
            if expected_path.ends_with("json") {
                "{\n  \"key\": \"訳\"\n}\n".as_bytes().to_vec()
            } else {
                "key=訳\n".as_bytes().to_vec()
            }
        );
        fs::remove_file(output).unwrap();
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pack_metadata_changes_shape_at_1_21_9() {
    let directory = temporary_directory();
    let entries = [translation("mod", "key", "訳", 0)];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    let mods = [ResourcePackMod {
        name: Some("mod".to_owned()),
        version: Some("1".to_owned()),
    }];
    for (version, modern) in [("1.21.8", false), ("1.21.9", true)] {
        let output = export_resource_pack(
            &project,
            ResourcePackOptions {
                mods: &mods,
                source_jar_filename: "mod.jar",
                minecraft_version: Some(version),
                manual_format: None,
            },
            &directory,
            false,
        )
        .unwrap();
        let metadata: serde_json::Value =
            serde_json::from_slice(&read_entry(&output, "pack.mcmeta")).unwrap();
        let pack = metadata.get("pack").unwrap();
        assert_eq!(
            pack.get("description").unwrap(),
            "mod Japanese Expansion Pack"
        );
        if modern {
            assert_eq!(pack.get("pack_format").unwrap(), 69);
            assert_eq!(pack.get("min_format").unwrap(), &serde_json::json!([69, 0]));
            assert_eq!(pack.get("max_format").unwrap(), &serde_json::json!([69, 0]));
        } else {
            assert_eq!(pack.get("pack_format").unwrap(), 64);
            assert!(pack.get("min_format").is_none());
            assert!(pack.get("max_format").is_none());
        }
        fs::remove_file(output).unwrap();
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unknown_resource_pack_version_accepts_manual_format() {
    let directory = temporary_directory();
    let entries = [translation("mod", "key", "訳", 0)];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );
    let mods = [ResourcePackMod {
        name: None,
        version: None,
    }];
    let output = export_resource_pack(
        &project,
        ResourcePackOptions {
            mods: &mods,
            source_jar_filename: "Example Mod.jar",
            minecraft_version: Some("1.22"),
            manual_format: Some(ManualPackFormat {
                major: 90,
                minor: 0,
            }),
        },
        &directory,
        false,
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_slice(&read_entry(&output, "pack.mcmeta")).unwrap();
    assert_eq!(
        metadata["pack"]["description"],
        "Example Mod Japanese Expansion Pack"
    );
    assert_eq!(metadata["pack"]["pack_format"], 90);
    assert_eq!(metadata["pack"]["min_format"], serde_json::json!([90, 0]));
    assert_eq!(metadata["pack"]["max_format"], serde_json::json!([90, 0]));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn resource_pack_filename_uses_first_mod_and_sanitizes_windows_components() {
    let normal_mods = [ResourcePackMod {
        name: Some("Create".to_owned()),
        version: Some("6.0.10".to_owned()),
    }];
    assert_eq!(
        resource_pack_filename(&normal_mods, "source.jar"),
        "Create-6.0.10-JP_EX.zip"
    );

    let invalid_name_mods = [ResourcePackMod {
        name: Some("Bad<>:\"/\\|?*\u{0001} . ".to_owned()),
        version: Some("1.21.1-1.3.4".to_owned()),
    }];
    assert_eq!(
        resource_pack_filename(&invalid_name_mods, "source.jar"),
        "Bad_________-1.21.1-1.3.4-JP_EX.zip"
    );

    let missing_name_mods = [ResourcePackMod {
        name: None,
        version: Some("1.0".to_owned()),
    }];
    assert_eq!(
        resource_pack_filename(&missing_name_mods, "Example Mod.jar"),
        "Example Mod-1.0-JP_EX.zip"
    );

    let missing_version_mods = [ResourcePackMod {
        name: Some("Create".to_owned()),
        version: None,
    }];
    assert_eq!(
        resource_pack_filename(&missing_version_mods, "source.jar"),
        "Create-JP_EX.zip"
    );

    let multiple_mods = [
        ResourcePackMod {
            name: Some("First Mod".to_owned()),
            version: Some("1.2".to_owned()),
        },
        ResourcePackMod {
            name: Some("Second Mod".to_owned()),
            version: Some("9.9".to_owned()),
        },
    ];
    assert_eq!(
        resource_pack_filename(&multiple_mods, "source.jar"),
        "First Mod-1.2-JP_EX.zip"
    );
}

#[test]
fn newest_confirmed_pack_format_and_default_jar_filename_are_available() {
    let format = pack_format_for_version("26.3").unwrap();
    assert_eq!((format.major, format.minor), (97, 1));
    assert_eq!(format.metadata, crate::PackMetadataFormat::FormatRange);
    assert_eq!(
        modified_jar_filename("example-1.2.jar").as_deref(),
        Some("example-1.2-ja.jar")
    );
}

#[test]
fn modified_jar_keeps_json_source_format_when_legacy_japanese_exists() {
    let directory = temporary_directory();
    let source_path = directory.join("mixed.jar");
    let output_path = directory.join("mixed-ja.jar");
    let legacy = b"key=old legacy\n";
    fs::write(
        &source_path,
        zip_bytes(&[
            (
                "assets/mod/lang/en_us.json",
                br#"{"key":"source"}"#,
                CompressionMethod::Stored,
            ),
            (
                "assets/mod/lang/ja_jp.lang",
                legacy,
                CompressionMethod::Stored,
            ),
        ]),
    )
    .unwrap();
    let entries = [translation("mod", "key", "updated", 0)];
    let namespaces = [NamespaceExport {
        namespace: "mod".into(),
        included: true,
    }];
    let files = [
        discovered("mod", "en_us", LanguageFileFormat::Json, "", "en_us.json"),
        discovered(
            "mod",
            "ja_jp",
            LanguageFileFormat::LegacyLang,
            "",
            "ja_jp.lang",
        ),
    ];
    let project = input(&entries, &namespaces, &files, &[], &[], "en_us");
    let artifacts = create_language_files(&project).unwrap();
    assert_eq!(artifacts[0].file_name, "ja_jp.json");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&artifacts[0].bytes).unwrap()["key"],
        "updated"
    );
    export_modified_jar(&project, &source_path, &output_path, false).unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&read_entry(&output_path, "assets/mod/lang/ja_jp.json")).unwrap();
    assert_eq!(json["key"], "updated");
    assert_eq!(
        read_entry(&output_path, "assets/mod/lang/ja_jp.lang"),
        legacy
    );
    assert_eq!(
        read_entry(&source_path, "assets/mod/lang/en_us.json"),
        br#"{"key":"source"}"#
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn modified_jar_replaces_adds_and_raw_copies_without_changing_source() {
    let directory = temporary_directory();
    let source_path = directory.join("original.jar");
    let output_path = directory.join("original-ja.jar");
    let nested_bytes = zip_bytes(&[
        (
            "assets/nested/lang/en_us.json",
            br#"{"key":"source"}"#,
            CompressionMethod::Deflated,
        ),
        (
            "assets/nested/lang/ja_jp.json",
            br#"{"key":"old nested"}"#,
            CompressionMethod::Stored,
        ),
    ]);
    let source_bytes = zip_bytes(&[
        (
            "assets/mod/lang/en_us.json",
            br#"{"key":"source"}"#,
            CompressionMethod::Deflated,
        ),
        (
            "assets/mod/lang/ja_jp.json",
            br#"{"key":"old"}"#,
            CompressionMethod::Stored,
        ),
        (
            "keep/stored.bin",
            b"stored bytes",
            CompressionMethod::Stored,
        ),
        (
            "keep/deflated.bin",
            b"deflated bytes",
            CompressionMethod::Deflated,
        ),
        (
            "META-INF/jars/library.jar",
            &nested_bytes,
            CompressionMethod::Stored,
        ),
    ]);
    fs::write(&source_path, source_bytes).unwrap();
    let original_hash = sha256_file(&source_path).unwrap();

    let entries = [
        translation("mod", "key", "updated", 0),
        translation("newmod", "new.key", "added", 1),
        translation("nested", "key", "nested translation", 2),
    ];
    let namespaces = [
        NamespaceExport {
            namespace: "mod".to_owned(),
            included: true,
        },
        NamespaceExport {
            namespace: "nested".to_owned(),
            included: true,
        },
        NamespaceExport {
            namespace: "newmod".to_owned(),
            included: true,
        },
    ];
    let files = [
        discovered("mod", "en_us", LanguageFileFormat::Json, "", "en_us.json"),
        discovered("mod", "ja_jp", LanguageFileFormat::Json, "", "ja_jp.json"),
        discovered(
            "nested",
            "en_us",
            LanguageFileFormat::Json,
            "META-INF/jars/library.jar",
            "en_us.json",
        ),
        discovered(
            "nested",
            "ja_jp",
            LanguageFileFormat::Json,
            "META-INF/jars/library.jar",
            "ja_jp.json",
        ),
        discovered(
            "newmod",
            "en_us",
            LanguageFileFormat::Json,
            "",
            "en_us.json",
        ),
    ];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    assert!(matches!(
        export_modified_jar(&project, &source_path, &source_path, true,),
        Err(ExportError::SourceAndOutputSame(_))
    ));
    assert_eq!(sha256_file(&source_path).unwrap(), original_hash);

    export_modified_jar(&project, &source_path, &output_path, false).unwrap();
    assert_eq!(
        read_entry(&output_path, "assets/mod/lang/ja_jp.json"),
        br#"{
  "key": "updated"
}
"#
    );
    assert_eq!(
        read_entry(&output_path, "assets/nested/lang/ja_jp.json"),
        br#"{
  "key": "nested translation"
}
"#
    );
    assert_eq!(
        read_entry(&output_path, "assets/newmod/lang/ja_jp.json"),
        br#"{
  "new.key": "added"
}
"#
    );
    assert_eq!(
        read_raw_entry(&output_path, "assets/newmod/lang/ja_jp.json").1,
        CompressionMethod::Deflated
    );
    assert_eq!(read_entry(&output_path, "keep/stored.bin"), b"stored bytes");
    assert_eq!(
        read_entry(&output_path, "keep/deflated.bin"),
        b"deflated bytes"
    );
    assert_eq!(
        read_entry(&output_path, "META-INF/jars/library.jar"),
        nested_bytes
    );
    assert_eq!(
        read_raw_entry(&source_path, "keep/stored.bin"),
        read_raw_entry(&output_path, "keep/stored.bin")
    );
    assert_eq!(
        read_raw_entry(&source_path, "keep/deflated.bin"),
        read_raw_entry(&output_path, "keep/deflated.bin")
    );
    assert_eq!(
        read_raw_entry(&source_path, "META-INF/jars/library.jar"),
        read_raw_entry(&output_path, "META-INF/jars/library.jar")
    );
    assert_eq!(sha256_file(&source_path).unwrap(), original_hash);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn signed_modified_jar_is_rejected_and_nested_target_is_a_warning() {
    let directory = temporary_directory();
    let source_path = directory.join("signed.jar");
    let output_path = directory.join("signed-ja.jar");
    let nested_bytes = zip_bytes(&[(
        "assets/nested/lang/ja_jp.json",
        br#"{"key":"old"}"#,
        CompressionMethod::Stored,
    )]);
    let source_bytes = zip_bytes(&[
        (
            "assets/mod/lang/en_us.json",
            br#"{"key":"source"}"#,
            CompressionMethod::Stored,
        ),
        ("META-INF/TEST.SF", b"signature", CompressionMethod::Stored),
        (
            "META-INF/jars/nested.jar",
            &nested_bytes,
            CompressionMethod::Stored,
        ),
    ]);
    fs::write(&source_path, source_bytes).unwrap();
    let entries = [
        translation("mod", "key", "訳", 0),
        translation("nested", "key", "訳", 1),
    ];
    let namespaces = [
        NamespaceExport {
            namespace: "mod".to_owned(),
            included: true,
        },
        NamespaceExport {
            namespace: "nested".to_owned(),
            included: true,
        },
    ];
    let files = [
        discovered("mod", "en_us", LanguageFileFormat::Json, "", "en_us.json"),
        discovered(
            "nested",
            "en_us",
            LanguageFileFormat::Json,
            "META-INF/jars/nested.jar",
            "en_us.json",
        ),
        discovered(
            "nested",
            "ja_jp",
            LanguageFileFormat::Json,
            "META-INF/jars/nested.jar",
            "ja_jp.json",
        ),
    ];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    assert!(matches!(
        export_modified_jar(&project, &source_path, &output_path, false,),
        Err(ExportError::SignedJar(_))
    ));
    assert!(!output_path.exists());

    let unsigned_path = directory.join("unsigned.jar");
    let unsigned = zip_bytes(&[
        (
            "assets/mod/lang/en_us.json",
            br#"{"key":"source"}"#,
            CompressionMethod::Stored,
        ),
        (
            "META-INF/jars/nested.jar",
            &nested_bytes,
            CompressionMethod::Stored,
        ),
    ]);
    fs::write(&unsigned_path, unsigned).unwrap();
    let request = ExportValidationRequest {
        input: &project,
        kind: ExportKind::ModifiedJar,
        minecraft_version: None,
        manual_pack_format: None,
        original_jar_path: Some(&unsigned_path),
    };
    let warnings = validate_export(&request);
    assert!(warnings.iter().any(|issue| {
        issue.code == ValidationIssueCode::NestedJarAlreadyHasTarget
            && issue.severity == ValidationSeverity::Warning
    }));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn validation_reports_missing_version_duplicate_keys_and_placeholder_warnings() {
    let entries = [
        translation("mod", "same", "one", 0),
        translation("mod", "same", "two", 1),
        translation("mod", "empty", "", 2),
        translation("mod", "whitespace", "　 \t", 3),
    ];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let malformed = ["broken.json".to_owned()];
    let placeholders = [PlaceholderWarning {
        namespace: "mod".to_owned(),
        key: "same".to_owned(),
    }];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &malformed,
        &placeholders,
        "en_us",
    );
    let request = ExportValidationRequest {
        input: &project,
        kind: ExportKind::ResourcePack,
        minecraft_version: None,
        manual_pack_format: None,
        original_jar_path: None,
    };
    let issues = validate_export(&request);
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == ValidationIssueCode::MissingMinecraftVersion)
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == ValidationIssueCode::DuplicateKeys)
    );
    assert!(issues.iter().any(|issue| {
        issue.code == ValidationIssueCode::UntranslatedEntries && issue.message.starts_with("2 ")
    }));
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == ValidationIssueCode::PlaceholderWarning)
    );
    assert!(issues.iter().any(|issue| {
        issue.code == ValidationIssueCode::MalformedTranslationFile
            && issue.severity == ValidationSeverity::Warning
    }));
    let unknown = ExportValidationRequest {
        input: &project,
        kind: ExportKind::ResourcePack,
        minecraft_version: Some("1.22"),
        manual_pack_format: None,
        original_jar_path: None,
    };
    assert!(validate_export(&unknown).iter().any(|issue| {
        issue.code == ValidationIssueCode::UnknownPackFormat
            && issue.severity == ValidationSeverity::Error
    }));
}

#[test]
fn failed_mid_write_removes_temporary_file_and_leaves_no_final_file() {
    let directory = temporary_directory();
    let output = directory.join("incomplete.zip");
    let result = crate::atomic_write_file(
        &output,
        false,
        |file| {
            file.write_all(b"partial output")?;
            Err(ExportError::Io(std::io::Error::other(
                "injected write failure",
            )))
        },
        |_| Ok(()),
    );
    assert!(result.is_err());
    assert!(!output.exists());
    let leftover_temporary_files: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".yc-export-"))
        .collect();
    assert!(leftover_temporary_files.is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn language_file_outputs_respect_overwrite_argument() {
    let directory = temporary_directory();
    let path = directory.join("mod").join("ja_jp.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"old").unwrap();
    let artifact = [LanguageFileArtifact {
        namespace: "mod".to_owned(),
        file_name: "ja_jp.json".to_owned(),
        bytes: b"new".to_vec(),
    }];
    assert!(write_language_files(&directory, &artifact, false).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"old");
    write_language_files(&directory, &artifact, true).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn merged_resource_pack_merges_namespaces_and_keeps_the_first_translation() {
    let directory = temporary_directory();
    let first_entries = [
        translation("shared", "collision", "先の訳", 0),
        translation("shared", "same", "同じ訳", 1),
        translation("first", "only.first", "最初のmod", 2),
    ];
    let first_namespaces = [
        NamespaceExport {
            namespace: "first".to_owned(),
            included: true,
        },
        NamespaceExport {
            namespace: "shared".to_owned(),
            included: true,
        },
    ];
    let first_files = [
        discovered("first", "en_us", LanguageFileFormat::Json, "", "en_us.json"),
        discovered(
            "shared",
            "en_us",
            LanguageFileFormat::Json,
            "",
            "en_us.json",
        ),
    ];
    let second_entries = [
        translation("shared", "collision", "後の訳", 0),
        translation("shared", "same", "同じ訳", 1),
        translation("second", "only.second", "二番目のmod", 2),
    ];
    let second_namespaces = [
        NamespaceExport {
            namespace: "second".to_owned(),
            included: true,
        },
        NamespaceExport {
            namespace: "shared".to_owned(),
            included: true,
        },
    ];
    let second_files = [
        discovered(
            "second",
            "en_us",
            LanguageFileFormat::Json,
            "",
            "en_us.json",
        ),
        discovered(
            "shared",
            "en_us",
            LanguageFileFormat::Json,
            "",
            "en_us.json",
        ),
    ];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let mods = [
        MergedPackMod {
            input: input(
                &first_entries,
                &first_namespaces,
                &first_files,
                &empty,
                &no_placeholders,
                "en_us",
            ),
            info: ResourcePackMod {
                name: Some("First".to_owned()),
                version: None,
            },
            label: "First Mod",
        },
        MergedPackMod {
            input: input(
                &second_entries,
                &second_namespaces,
                &second_files,
                &empty,
                &no_placeholders,
                "en_us",
            ),
            info: ResourcePackMod {
                name: Some("Second".to_owned()),
                version: None,
            },
            label: "Second Mod",
        },
    ];
    assert_eq!(
        merged_resource_pack_filename(&mods, "Fabulously Optimized"),
        "Fabulously Optimized-JP_EX.zip"
    );
    assert_eq!(
        merged_resource_pack_filename(&mods, "Project Name"),
        "Project Name-JP_EX.zip"
    );
    assert_eq!(
        merged_resource_pack_filename(&mods, "12 個の mod"),
        "YakuCraft-JP_EX.zip"
    );
    assert_eq!(
        merged_resource_pack_filename(&mods, "Pack<>/Name"),
        "Pack___Name-JP_EX.zip"
    );

    let output = directory.join("merged.zip");
    let report = export_merged_resource_pack(
        &mods,
        MergedResourcePackOptions {
            minecraft_version: Some("1.20.4"),
            manual_format: None,
            pack_description: Some("Fabulously Optimized の日本語訳（2 mod）"),
        },
        &output,
        false,
    )
    .unwrap();
    assert_eq!(report.output_path, output);
    assert_eq!(
        report.conflicts,
        [crate::MergedResourcePackConflict {
            namespace: "shared".to_owned(),
            key: "collision".to_owned(),
            kept_from: "First Mod".to_owned(),
            dropped_from: "Second Mod".to_owned(),
        }]
    );
    assert_eq!(
        read_entry(&output, "assets/shared/lang/ja_jp.json"),
        "{\n  \"collision\": \"先の訳\",\n  \"same\": \"同じ訳\"\n}\n".as_bytes()
    );
    assert_eq!(
        read_entry(&output, "assets/first/lang/ja_jp.json"),
        "{\n  \"only.first\": \"最初のmod\"\n}\n".as_bytes()
    );
    assert_eq!(
        read_entry(&output, "assets/second/lang/ja_jp.json"),
        "{\n  \"only.second\": \"二番目のmod\"\n}\n".as_bytes()
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&read_entry(&output, "pack.mcmeta")).unwrap();
    assert_eq!(
        metadata["pack"]["description"],
        "Fabulously Optimized の日本語訳（2 mod）"
    );

    let single = [MergedPackMod {
        input: mods[0].input.clone(),
        info: ResourcePackMod {
            name: Some("First".to_owned()),
            version: Some("1.2".to_owned()),
        },
        label: "first.jar",
    }];
    assert_eq!(
        merged_resource_pack_filename(&single, "Ignored for one mod"),
        resource_pack_filename(&[single[0].info.clone()], single[0].label)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn merged_resource_pack_exports_translations_when_source_locale_file_is_missing() {
    let directory = temporary_directory();
    let entries = [translation("other_language_only", "key", "訳", 0)];
    let namespaces = [
        NamespaceExport {
            namespace: "empty".to_owned(),
            included: true,
        },
        NamespaceExport {
            namespace: "other_language_only".to_owned(),
            included: true,
        },
    ];
    let files = [discovered(
        "other_language_only",
        "fr_fr",
        LanguageFileFormat::Json,
        "",
        "fr_fr.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let mods = [MergedPackMod {
        input: input(
            &entries,
            &namespaces,
            &files,
            &empty,
            &no_placeholders,
            "en_us",
        ),
        info: ResourcePackMod::default(),
        label: "Example Mod",
    }];
    let output = directory.join("merged.zip");

    export_merged_resource_pack(
        &mods,
        MergedResourcePackOptions {
            minecraft_version: Some("1.20.4"),
            manual_format: None,
            pack_description: None,
        },
        &output,
        false,
    )
    .unwrap();

    assert_eq!(
        read_entry(&output, "assets/other_language_only/lang/ja_jp.json"),
        "{\n  \"key\": \"訳\"\n}\n".as_bytes()
    );
    let archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
    assert!(
        archive
            .file_names()
            .all(|name| name.is_ok_and(|name| name != "assets/empty/lang/ja_jp.json"))
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn language_file_and_modified_jar_use_available_locale_when_source_is_missing() {
    let directory = temporary_directory();
    let entries = [translation("mod", "key", "訳", 0)];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "fr_fr",
        LanguageFileFormat::Json,
        "",
        "fr_fr.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    let artifacts = create_language_files(&project).unwrap();
    assert_eq!(artifacts[0].file_name, "ja_jp.json");
    assert_eq!(artifacts[0].bytes, "{\n  \"key\": \"訳\"\n}\n".as_bytes());

    let source = directory.join("source.jar");
    let output = directory.join("translated.jar");
    fs::write(
        &source,
        zip_bytes(&[(
            "assets/mod/lang/fr_fr.json",
            b"{\"key\":\"source\"}",
            CompressionMethod::Deflated,
        )]),
    )
    .unwrap();
    export_modified_jar(&project, &source, &output, false).unwrap();
    assert_eq!(
        read_entry(&output, "assets/mod/lang/ja_jp.json"),
        artifacts[0].bytes
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn legacy_resource_pack_preserves_the_source_parse_escapes_marker() {
    let directory = temporary_directory();
    let entries = [translation("mod", "key", "plain", 0)];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::LegacyLang,
        "",
        "en_us.lang",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let parse_escapes_namespaces = ["mod".to_owned()];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );
    let project = ExportInput {
        legacy_parse_escapes_namespaces: &parse_escapes_namespaces,
        ..project
    };
    let output = export_resource_pack(
        &project,
        ResourcePackOptions {
            mods: &[],
            source_jar_filename: "mod.jar",
            minecraft_version: Some("1.12.2"),
            manual_format: None,
        },
        &directory,
        false,
    )
    .unwrap();

    assert_eq!(
        read_entry(&output, "assets/mod/lang/ja_jp.lang"),
        b"#PARSE_ESCAPES\nkey=plain\n"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn merged_resource_pack_uses_pack_format_across_source_formats_and_keeps_valid_data_when_a_locale_is_malformed()
 {
    let directory = temporary_directory();
    let entries = [translation("shared", "key", "訳", 0)];
    let namespaces = [NamespaceExport {
        namespace: "shared".to_owned(),
        included: true,
    }];
    let json_files = [discovered(
        "shared",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let legacy_files = [discovered(
        "shared",
        "en_us",
        LanguageFileFormat::LegacyLang,
        "",
        "en_us.lang",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let mods = [
        MergedPackMod {
            input: input(
                &entries,
                &namespaces,
                &json_files,
                &empty,
                &no_placeholders,
                "en_us",
            ),
            info: ResourcePackMod::default(),
            label: "JSON Mod",
        },
        MergedPackMod {
            input: input(
                &entries,
                &namespaces,
                &legacy_files,
                &empty,
                &no_placeholders,
                "en_us",
            ),
            info: ResourcePackMod::default(),
            label: "Legacy Mod",
        },
    ];
    let output = directory.join("mixed.zip");
    export_merged_resource_pack(
        &mods,
        MergedResourcePackOptions {
            minecraft_version: Some("1.20.4"),
            manual_format: None,
            pack_description: None,
        },
        &output,
        false,
    )
    .unwrap();
    assert_eq!(
        read_entry(&output, "assets/shared/lang/ja_jp.json"),
        "{\n  \"key\": \"訳\"\n}\n".as_bytes()
    );
    fs::remove_file(&output).unwrap();

    let malformed = ["broken.json".to_owned()];
    let invalid_mod = [MergedPackMod {
        input: input(
            &entries,
            &namespaces,
            &json_files,
            &malformed,
            &no_placeholders,
            "en_us",
        ),
        info: ResourcePackMod::default(),
        label: "Broken Mod",
    }];
    assert!(
        export_merged_resource_pack(
            &invalid_mod,
            MergedResourcePackOptions {
                minecraft_version: Some("1.20.4"),
                manual_format: None,
                pack_description: None,
            },
            &output,
            false,
        )
        .is_ok()
    );
    assert!(output.exists());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn staged_resource_pack_installs_to_selected_name_and_preserves_output_on_failure() {
    let directory = temporary_directory();
    let staging = directory.join(".yakucraft-export-first.zip");
    let destination = directory.join("instance/resourcepacks/translations.zip");
    fs::write(&staging, b"first pack").unwrap();
    assert_eq!(
        install_resource_pack_to(&staging, &destination, false).unwrap(),
        destination
    );
    let later_staging = directory.join(".yakucraft-export-second.zip");
    fs::write(&later_staging, b"updated pack").unwrap();
    assert!(matches!(
        install_resource_pack_to(&later_staging, &destination, false),
        Err(ExportError::OutputExists(path)) if path == destination
    ));
    assert_eq!(fs::read(&destination).unwrap(), b"first pack");
    install_resource_pack_to(&later_staging, &destination, true).unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"updated pack");
    fs::remove_file(&later_staging).unwrap();
    assert!(install_resource_pack_to(&later_staging, &destination, true).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"updated pack");
    assert_eq!(
        fs::read_dir(destination.parent().unwrap()).unwrap().count(),
        1
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn resource_pack_install_respects_overwrite_and_instance_detection() {
    let directory = temporary_directory();
    let pack = directory.join("pack.zip");
    fs::write(&pack, b"first pack").unwrap();
    let instance = directory.join("instance");
    let mods_dir = instance.join("mods");
    fs::create_dir_all(&mods_dir).unwrap();
    fs::write(instance.join("options.txt"), b"options").unwrap();
    assert_eq!(
        instance_dir_from_mods_dir(&mods_dir),
        Some(instance.clone())
    );
    assert!(looks_like_instance_dir(&instance));

    let installed = install_resource_pack(&pack, &instance, false).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), b"first pack");
    fs::write(&pack, b"replacement pack").unwrap();
    assert!(matches!(
        install_resource_pack(&pack, &instance, false),
        Err(ExportError::OutputExists(_))
    ));
    assert_eq!(fs::read(&installed).unwrap(), b"first pack");
    assert_eq!(
        install_resource_pack(&pack, &instance, true).unwrap(),
        installed
    );
    assert_eq!(fs::read(&installed).unwrap(), b"replacement pack");

    let non_instance = directory.join("not-instance");
    fs::create_dir_all(non_instance.join("mods")).unwrap();
    assert!(!looks_like_instance_dir(&non_instance));
    assert_eq!(
        instance_dir_from_mods_dir(&non_instance.join("custom-mods")),
        Some(non_instance)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn in_place_jar_replacement_backs_up_hashes_and_restores_original() {
    let directory = temporary_directory();
    let source_jar = directory.join("source.jar");
    let target_jar = directory.join("instance-mod.jar");
    let backup_dir = directory.join("backups");
    fs::write(
        &source_jar,
        zip_bytes(&[(
            "assets/mod/lang/en_us.json",
            br#"{"key":"source"}"#,
            CompressionMethod::Deflated,
        )]),
    )
    .unwrap();
    let previous_bytes = zip_bytes(&[
        (
            "assets/mod/lang/en_us.json",
            br#"{"key":"source"}"#,
            CompressionMethod::Deflated,
        ),
        (
            "assets/mod/lang/ja_jp.json",
            br#"{"key":"old"}"#,
            CompressionMethod::Stored,
        ),
    ]);
    fs::write(&target_jar, &previous_bytes).unwrap();
    let entries = [translation("mod", "key", "新しい訳", 0)];
    let namespaces = [NamespaceExport {
        namespace: "mod".to_owned(),
        included: true,
    }];
    let files = [discovered(
        "mod",
        "en_us",
        LanguageFileFormat::Json,
        "",
        "en_us.json",
    )];
    let empty: [String; 0] = [];
    let no_placeholders: [PlaceholderWarning; 0] = [];
    let project = input(
        &entries,
        &namespaces,
        &files,
        &empty,
        &no_placeholders,
        "en_us",
    );

    let report = replace_jar_in_place(&project, &source_jar, &target_jar, &backup_dir).unwrap();
    assert!(report.backup_path.is_file());
    assert_eq!(fs::read(&report.backup_path).unwrap(), previous_bytes);
    assert_eq!(
        report.previous_sha256,
        digest_hex(sha256_file(&report.backup_path).unwrap())
    );
    assert_eq!(report.written_sha256.len(), 64);
    assert_eq!(
        report.written_sha256,
        digest_hex(sha256_file(&target_jar).unwrap())
    );
    assert_ne!(report.previous_sha256, report.written_sha256);
    assert_eq!(
        read_entry(&target_jar, "assets/mod/lang/ja_jp.json"),
        "{\n  \"key\": \"新しい訳\"\n}\n".as_bytes()
    );

    restore_jar_from_backup(&report.backup_path, &target_jar).unwrap();
    assert_eq!(fs::read(&target_jar).unwrap(), previous_bytes);
    fs::remove_dir_all(directory).unwrap();
}

fn digest_hex(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
