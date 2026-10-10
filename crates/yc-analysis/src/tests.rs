use std::{
    fs,
    io::{Cursor, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use yc_language::{LanguageFileFormat, ReviewSettings};
use yc_metadata::LoaderKind;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::*;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TemporaryJar(PathBuf);

impl Drop for TemporaryJar {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temporary_jar(entries: &[(&str, &[u8])]) -> TemporaryJar {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("yc-analysis-test-{}-{id}.jar", std::process::id()));
    fs::write(&path, make_jar(entries)).expect("write fixture JAR");
    TemporaryJar(path)
}

fn temporary_directory() -> TemporaryDirectory {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("yc-analysis-scan-test-{}-{id}", std::process::id()));
    fs::create_dir_all(&path).expect("create fixture directory");
    TemporaryDirectory(path)
}

fn make_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, contents) in entries {
        writer
            .start_file(
                *path,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .expect("start fixture entry");
        writer.write_all(contents).expect("write fixture entry");
    }
    writer.finish().expect("finish fixture JAR").into_inner()
}

fn analyze(jar: &TemporaryJar) -> Result<AnalysisData, AnalysisError> {
    analyze_jar(&jar.0, &CancellationToken::default(), |_| {})
}

fn analyze_with_limits(
    jar: &TemporaryJar,
    limits: ArchiveLimits,
) -> Result<AnalysisData, AnalysisError> {
    analyze_jar_with_limits(&jar.0, limits, &CancellationToken::default(), |_| {})
}

fn patch_uncompressed_size(bytes: &mut [u8], path: &[u8], forged_size: u32) {
    let forged_size = forged_size.to_le_bytes();
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        if bytes[offset..offset + 4] == *b"PK\x03\x04" {
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
        } else if bytes[offset..offset + 4] == *b"PK\x01\x02" {
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
fn fabric_metadata_and_japanese_counts_are_available() {
    let jar = temporary_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"sample","name":"Sample Mod","version":"2.0","depends":{"minecraft":">=1.20"}}"#,
        ),
        (
            "assets/sample/lang/en_us.json",
            br#"{"key.one":"One","key.two":"Two"}"#,
        ),
        (
            "assets/sample/lang/ja_jp.json",
            r#"{"key.one":"一つ"}"#.as_bytes(),
        ),
        (
            "assets/sample/lang/zh_cn.json",
            r#"{"key.one":"一","key.two":"二","key.three":"三"}"#.as_bytes(),
        ),
        (
            "assets/sample_extra/lang/en_US.json",
            br#"{"other.key":"Other"}"#,
        ),
    ]);

    let data = analyze(&jar).expect("analyze fixture");
    assert_eq!(data.result.mods.len(), 1);
    assert_eq!(data.result.mods[0].mod_id.as_deref(), Some("sample"));
    assert_eq!(data.result.mods[0].loader, LoaderKind::Fabric);
    assert_eq!(
        data.result.recommended_source_locale.as_deref(),
        Some("en_us")
    );
    assert_eq!(data.result.existing_japanese_translation_count, 1);
    assert_eq!(data.result.missing_source_keys_count, 1);
    assert_eq!(data.result.namespaces.len(), 2);
    assert_eq!(data.result.language_files.len(), 4);

    let merged = data.merge_review(&data.default_review_settings());
    assert_eq!(merged.existing_japanese_translation_count, 1);
    assert_eq!(merged.missing_source_keys_count, 1);
    assert_eq!(merged.entries.len(), 4);
}

#[test]
fn legacy_forge_metadata_and_lang_format_are_parsed() {
    let jar = temporary_jar(&[
        (
            "mcmod.info",
            br#"[{"modid":"legacy","name":"Legacy Mod","version":"1.2","mcversion":"1.12.2"}]"#,
        ),
        (
            "assets/legacy/lang/en_US.lang",
            b"block.legacy.machine=Machine\nitem.legacy.tool=Tool\\nName\n",
        ),
    ]);

    let data = analyze(&jar).expect("analyze fixture");
    assert_eq!(data.result.mods[0].mod_id.as_deref(), Some("legacy"));
    assert_eq!(data.result.mods[0].loader, LoaderKind::LegacyForge);
    assert_eq!(data.result.language_files.len(), 1);
    assert_eq!(
        data.result.language_files[0].format,
        LanguageFileFormat::LegacyLang
    );
    assert_eq!(data.result.language_files[0].entry_count, 2);
}

#[test]
fn nested_language_namespace_is_excluded_by_default_and_can_be_included() {
    let nested_jar = make_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"nested","name":"Nested Mod","version":"1.0"}"#,
        ),
        (
            "assets/nested/lang/en_us.json",
            br#"{"nested.key":"Nested"}"#,
        ),
    ]);
    let root_jar = temporary_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"root","name":"Root Mod","version":"1.0","jars":[{"file":"META-INF/jars/dependency.jar"}]}"#,
        ),
        (
            "assets/root/lang/en_us.json",
            br#"{"root.key":"Root"}"#,
        ),
        ("META-INF/jars/dependency.jar", &nested_jar),
    ]);

    let data = analyze(&root_jar).expect("analyze fixture");
    let nested_namespace = data
        .result
        .namespaces
        .iter()
        .find(|namespace| namespace.namespace == "nested")
        .expect("nested namespace");
    assert!(!nested_namespace.included);
    assert_eq!(
        nested_namespace.container_path,
        "META-INF/jars/dependency.jar"
    );
    assert!(data.result.nested_archives[0].has_languages);

    let default_merge = data.merge_review(&data.default_review_settings());
    assert_eq!(default_merge.entries.len(), 1);
    let empty_selection = ReviewSettings {
        source_locale: "en_us".to_owned(),
        included_namespaces: Vec::new(),
        ..data.default_review_settings()
    };
    assert!(data.merge_review(&empty_selection).entries.is_empty());
    let settings = ReviewSettings {
        source_locale: "en_us".to_owned(),
        included_namespaces: vec!["root".to_owned(), "nested".to_owned()],
        ..data.default_review_settings()
    };
    let included_merge = data.merge_review(&settings);
    assert_eq!(included_merge.entries.len(), 2);
}

#[test]
fn nested_language_entry_with_forged_actual_size_is_rejected() {
    let oversized_contents = format!("{{\"large.key\":\"{}\"}}", "x".repeat(256 * 1024));
    let oversized_path = "assets/nested/lang/en_us.json";
    let mut nested_jar = make_jar(&[(oversized_path, oversized_contents.as_bytes())]);
    // ZIPが小さいサイズを申告しても実際の展開量が上限に達した時点で止まることを確認する。
    patch_uncompressed_size(&mut nested_jar, oversized_path.as_bytes(), 1);
    let metadata = br#"{"id":"root","jars":["META-INF/jars/lib.jar"]}"#;
    let root_language = br#"{"root.key":"Root"}"#;
    let jar = temporary_jar(&[
        ("fabric.mod.json", metadata),
        ("assets/root/lang/en_us.json", root_language),
        ("META-INF/jars/lib.jar", &nested_jar),
    ]);
    let limits = ArchiveLimits {
        max_text_size_bytes: 64,
        max_compression_ratio: u32::MAX,
        ..ArchiveLimits::default()
    };

    let data = analyze_with_limits(&jar, limits).expect("root language remains usable");
    let nested_language = data
        .result
        .language_files
        .iter()
        .find(|file| file.container_path == "META-INF/jars/lib.jar")
        .expect("nested language file is discovered");
    assert_eq!(nested_language.entry_count, 0);
    assert!(data.result.warnings.iter().any(|warning| {
        warning.container_path == "META-INF/jars/lib.jar"
            && warning.path == oversized_path
            && warning.message.contains("actual size exceeds 64 bytes")
    }));
}

#[test]
fn nested_language_entry_with_declared_size_over_limit_is_rejected() {
    let nested_path = "assets/nested/lang/en_us.json";
    let mut nested_jar = make_jar(&[(nested_path, br#"{"nested.key":"Nested"}"#)]);
    patch_uncompressed_size(&mut nested_jar, nested_path.as_bytes(), 128);
    let metadata = br#"{"id":"root","jars":["META-INF/jars/lib.jar"]}"#;
    let root_language = br#"{"root.key":"Root"}"#;
    let jar = temporary_jar(&[
        ("fabric.mod.json", metadata),
        ("assets/root/lang/en_us.json", root_language),
        ("META-INF/jars/lib.jar", &nested_jar),
    ]);
    let limits = ArchiveLimits {
        max_text_size_bytes: 64,
        max_compression_ratio: u32::MAX,
        ..ArchiveLimits::default()
    };

    let data = analyze_with_limits(&jar, limits).expect("root language remains usable");
    let nested_language = data
        .result
        .language_files
        .iter()
        .find(|file| file.container_path == "META-INF/jars/lib.jar")
        .expect("nested language file is discovered");
    assert_eq!(nested_language.entry_count, 0);
    assert!(data.result.warnings.iter().any(|warning| {
        warning.container_path == "META-INF/jars/lib.jar"
            && warning.path == nested_path
            && warning
                .message
                .contains("declared size 128 exceeds 64 bytes")
    }));
}

#[test]
fn root_and_nested_language_reads_share_the_cumulative_limit() {
    let nested_language = br#"{"nested.key":"Nested"}"#;
    let nested_jar = make_jar(&[("assets/nested/lang/en_us.json", nested_language)]);
    let metadata = br#"{"id":"root","jars":["META-INF/jars/lib.jar"]}"#;
    let root_language = br#"{"root.key":"Root"}"#;
    let jar = temporary_jar(&[
        ("fabric.mod.json", metadata),
        ("assets/root/lang/en_us.json", root_language),
        ("META-INF/jars/lib.jar", &nested_jar),
    ]);
    let limits = ArchiveLimits {
        max_cumulative_read_bytes: (metadata.len() + nested_jar.len() + root_language.len()) as u64,
        ..ArchiveLimits::default()
    };

    let data = analyze_with_limits(&jar, limits).expect("root language remains usable");
    let root_summary = data
        .result
        .language_files
        .iter()
        .find(|file| file.container_path.is_empty())
        .expect("root language file");
    let nested_summary = data
        .result
        .language_files
        .iter()
        .find(|file| file.container_path == "META-INF/jars/lib.jar")
        .expect("nested language file");
    assert_eq!(root_summary.entry_count, 1);
    assert_eq!(nested_summary.entry_count, 0);
    assert!(data.result.warnings.iter().any(|warning| {
        warning.container_path == "META-INF/jars/lib.jar"
            && warning.path == "assets/nested/lang/en_us.json"
            && warning.message.contains("cumulative read exceeds")
    }));
}

#[test]
fn root_values_win_and_nested_only_keys_are_unioned_for_shared_namespace() {
    let nested_jar = make_jar(&[(
        "assets/shared/lang/en_us.json",
        br#"{"shared.key":"Nested","nested.only":"Nested only"}"#,
    )]);
    let root_jar = temporary_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"root","name":"Root Mod","version":"1.0","jars":["META-INF/jars/dependency.jar"]}"#,
        ),
        (
            "assets/shared/lang/en_us.json",
            br#"{"shared.key":"Root"}"#,
        ),
        ("META-INF/jars/dependency.jar", &nested_jar),
    ]);

    let data = analyze(&root_jar).expect("analyze fixture");
    let entries = data.merge_review(&data.default_review_settings()).entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.key == "shared.key")
            .and_then(|entry| entry.source_text.as_deref()),
        Some("Root")
    );
    assert!(entries.iter().any(|entry| entry.key == "nested.only"));
    let shared_namespaces: Vec<_> = data
        .result
        .namespaces
        .iter()
        .filter(|namespace| namespace.namespace == "shared")
        .collect();
    assert_eq!(shared_namespaces.len(), 1);
    assert!(shared_namespaces[0].container_path.is_empty());
    assert!(shared_namespaces[0].included);
}

#[test]
fn missing_metadata_does_not_stop_language_analysis() {
    let jar = temporary_jar(&[(
        "assets/unknown/lang/en_us.json",
        br#"{"unknown.key":"Value"}"#,
    )]);

    let data = analyze(&jar).expect("analyze fixture without metadata");
    assert_eq!(data.result.mods[0].loader, LoaderKind::Unknown);
    assert_eq!(data.result.language_files.len(), 1);
    assert!(
        data.result
            .warnings
            .iter()
            .any(|warning| { warning.message.contains("Loader") })
    );
}

#[test]
fn no_language_resources_is_fatal() {
    let jar = temporary_jar(&[("fabric.mod.json", br#"{"id":"empty"}"#)]);
    let error = analyze(&jar).expect_err("language-free JAR must fail");
    assert!(matches!(error, AnalysisError::NoLanguageFiles));
}

#[test]
fn cancellation_stops_during_zip_path_scan() {
    let mut contents = Vec::with_capacity(700);
    for index in 0..700 {
        let path = format!("META-INF/entry-{index}.txt");
        contents.push((path, b"x".as_slice()));
    }
    contents.push((
        "assets/sample/lang/en_us.json".to_owned(),
        br#"{"key":"value"}"#.as_slice(),
    ));
    let entries: Vec<_> = contents
        .iter()
        .map(|(path, bytes)| (path.as_str(), *bytes))
        .collect();
    let jar = temporary_jar(&entries);
    let cancellation = CancellationToken::default();
    let callback_token = cancellation.clone();

    let error = analyze_jar(&jar.0, &cancellation, move |progress| {
        if progress.stage == AnalysisStage::ScanZipPaths && progress.current == 1 {
            callback_token.cancel();
        }
    })
    .expect_err("cancelled analysis must stop");
    assert!(matches!(error, AnalysisError::Cancelled));
}

#[test]
fn progress_reports_every_analysis_stage() {
    let jar = temporary_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"sample","name":"Sample","version":"1.0"}"#,
        ),
        (
            "assets/sample/lang/en_us.json",
            br#"{"sample.key":"Value"}"#,
        ),
    ]);
    let mut stages = BTreeSet::new();

    analyze_jar(&jar.0, &CancellationToken::default(), |progress| {
        stages.insert(progress.stage);
    })
    .expect("analyze fixture");

    assert_eq!(
        stages,
        [
            AnalysisStage::ValidateJar,
            AnalysisStage::Sha256,
            AnalysisStage::ScanZipPaths,
            AnalysisStage::ParseMetadata,
            AnalysisStage::ScanNestedJars,
            AnalysisStage::DiscoverNamespaces,
            AnalysisStage::ParseLanguages,
            AnalysisStage::DetectTargetTranslation,
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn lightweight_scan_reports_untranslated_partial_and_complete_mods() {
    let untranslated = temporary_jar(&[("assets/plain/lang/en_us.json", br#"{"one":"1"}"#)]);
    let partial = temporary_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"sample","name":"Sample","version":"2.0"}"#,
        ),
        ("assets/sample/lang/en_us.json", br#"{"one":"1","two":"2"}"#),
        (
            "assets/sample/lang/ja_jp.json",
            r#"{"one":"一","unrelated":"別"}"#.as_bytes(),
        ),
    ]);
    let complete = temporary_jar(&[
        (
            "assets/complete/lang/en_us.json",
            br#"{"one":"1","two":"2"}"#,
        ),
        (
            "assets/complete/lang/ja_jp.json",
            r#"{"one":"一","two":"二","extra":"追加"}"#.as_bytes(),
        ),
    ]);
    let options = ScanOptions::default();

    let untranslated = scan_mod_jar(&untranslated.0, &options);
    assert!(matches!(
        untranslated.status,
        ModLanguageStatus::Untranslated
    ));
    assert_eq!(untranslated.source_key_count, 1);
    assert_eq!(untranslated.translated_key_count, 0);

    let partial = scan_mod_jar(&partial.0, &options);
    assert_eq!(partial.mods[0].mod_id.as_deref(), Some("sample"));
    assert_eq!(partial.mods[0].name.as_deref(), Some("Sample"));
    assert_eq!(partial.mods[0].version.as_deref(), Some("2.0"));
    assert_eq!(partial.mods[0].loader, LoaderKind::Fabric);
    assert_eq!(partial.source_key_count, 2);
    assert_eq!(partial.translated_key_count, 1);
    assert!(matches!(
        partial.status,
        ModLanguageStatus::Partial { coverage } if coverage == 0.5
    ));
    assert!(partial.file_size_bytes.is_some());
    assert!(partial.sha256.is_some());

    let complete = scan_mod_jar(&complete.0, &options);
    assert_eq!(complete.source_key_count, 2);
    assert_eq!(complete.translated_key_count, 2);
    assert!(matches!(complete.status, ModLanguageStatus::Complete));
}

#[test]
fn lightweight_scan_and_analysis_ignore_blank_japanese_values() {
    let jar = temporary_jar(&[
        (
            "assets/sample/lang/en_us.json",
            br#"{"one":"One","two":"Two","empty.source":""}"#,
        ),
        (
            "assets/sample/lang/ja_jp.json",
            r#"{"one":"","two":"　","empty.source":"訳"}"#.as_bytes(),
        ),
    ]);

    let scan = scan_mod_jar(&jar.0, &ScanOptions::default());
    assert_eq!(scan.source_key_count, 3);
    assert_eq!(scan.translated_key_count, 1);
    assert!(matches!(
        scan.status,
        ModLanguageStatus::Partial { coverage } if (coverage - 1.0 / 3.0).abs() < f64::EPSILON
    ));

    let analysis = analyze(&jar).expect("analyze fixture");
    assert_eq!(analysis.result.existing_japanese_translation_count, 1);
    assert_eq!(
        analysis
            .merge_review(&analysis.default_review_settings())
            .existing_japanese_translation_count,
        1
    );

    let blank_only = temporary_jar(&[
        ("assets/blank/lang/en_us.json", br#"{"key":"Source"}"#),
        ("assets/blank/lang/ja_jp.json", r#"{"key":"　"}"#.as_bytes()),
    ]);
    let blank_scan = scan_mod_jar(&blank_only.0, &ScanOptions::default());
    assert_eq!(blank_scan.translated_key_count, 0);
    assert!(matches!(blank_scan.status, ModLanguageStatus::Untranslated));
}

#[test]
fn lightweight_scan_reports_no_language_files_and_broken_archives() {
    let no_languages = temporary_jar(&[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n")]);
    let broken = temporary_jar(&[]);
    fs::write(&broken.0, b"not a zip archive").expect("replace fixture with broken JAR");
    let options = ScanOptions::default();

    let no_languages = scan_mod_jar(&no_languages.0, &options);
    assert!(matches!(
        no_languages.status,
        ModLanguageStatus::NoLanguageFiles
    ));

    let broken = scan_mod_jar(&broken.0, &options);
    assert!(matches!(broken.status, ModLanguageStatus::Failed { .. }));
    assert!(broken.sha256.is_some());
}

#[test]
fn lightweight_scan_parses_legacy_lang_files() {
    let jar = temporary_jar(&[
        (
            "assets/legacy/lang/en_US.lang",
            b"first=First\nsecond=Second\n",
        ),
        ("assets/legacy/lang/ja_JP.lang", b"first=\xe4\xb8\x80\n"),
    ]);

    let result = scan_mod_jar(&jar.0, &ScanOptions::default());

    assert_eq!(result.source_key_count, 2);
    assert_eq!(result.translated_key_count, 1);
    assert!(matches!(
        result.status,
        ModLanguageStatus::Partial { coverage } if coverage == 0.5
    ));
}

#[test]
fn lightweight_scan_uses_largest_language_and_includes_nested_jar_languages() {
    let nested = make_jar(&[
        (
            "assets/dependency/lang/en_us.json",
            br#"{"nested":"source"}"#,
        ),
        (
            "assets/dependency/lang/ja_jp.json",
            br#"{"nested":"translation"}"#,
        ),
    ]);
    let jar = temporary_jar(&[
        (
            "assets/sample/lang/fr_fr.json",
            br#"{"one":"1","two":"2","three":"3"}"#,
        ),
        (
            "assets/sample/lang/de_de.json",
            br#"{"other.one":"1","other.two":"2"}"#,
        ),
        (
            "assets/sample/lang/ja_jp.json",
            r#"{"one":"一","unrelated":"別"}"#.as_bytes(),
        ),
        ("META-INF/jars/dependency.jar", &nested),
    ]);

    let result = scan_mod_jar(&jar.0, &ScanOptions::default());

    assert_eq!(result.source_key_count, 4);
    assert_eq!(result.translated_key_count, 2);
    assert!(matches!(
        result.status,
        ModLanguageStatus::Partial { coverage } if coverage == 0.5
    ));
}

#[test]
fn lightweight_scan_keeps_root_languages_when_nested_discovery_exhausts_budget() {
    // Stored ZIP payloads make the bundled JAR exceed the scanner's shared 16 MiB budget.
    // This also covers exhaustion from multiple individually smaller dependencies.
    for sizes in [vec![17 * 1024 * 1024], vec![9 * 1024 * 1024; 2]] {
        let dependencies: Vec<_> = sizes
            .into_iter()
            .map(|size| {
                let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
                writer
                    .start_file(
                        "payload.bin",
                        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                    )
                    .expect("start stored payload");
                writer.write_all(&vec![0_u8; size]).expect("write payload");
                let bytes = writer.finish().expect("finish dependency").into_inner();
                assert!(bytes.len() >= size);
                bytes
            })
            .collect();
        let mut entries = vec![
            (
                "assets/example/lang/en_us.json".to_owned(),
                br#"{"key":"Source"}"#.to_vec(),
            ),
            (
                "assets/example/lang/ja_jp.json".to_owned(),
                br#"{"key":"Translation"}"#.to_vec(),
            ),
        ];
        for (index, bytes) in dependencies.into_iter().enumerate() {
            entries.push((format!("META-INF/jars/dependency{index}.jar"), bytes));
        }
        let refs: Vec<_> = entries
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        let jar = temporary_jar(&refs);
        let result = scan_mod_jar(&jar.0, &ScanOptions::default());
        assert_eq!(result.source_key_count, 1);
        assert_eq!(result.translated_key_count, 1);
        assert_eq!(result.status, ModLanguageStatus::Complete);
    }
}

#[test]
fn lightweight_folder_scan_discovers_nested_only_languages() {
    let directory = temporary_directory();
    let nested = make_jar(&[("assets/example/lang/en_us.json", br#"{"key":"Source"}"#)]);
    fs::write(
        directory.0.join("outer.jar"),
        make_jar(&[("META-INF/jars/inner.jar", &nested)]),
    )
    .unwrap();
    let results = scan_mods_folder(
        &directory.0,
        &ScanOptions::default(),
        &CancellationToken::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_key_count, 1);
    assert_eq!(results[0].status, ModLanguageStatus::Untranslated);
}

#[test]
fn lightweight_scan_applies_text_limit_to_nested_languages() {
    let path = "assets/example/lang/en_us.json";
    let mut nested = make_jar(&[(path, br#"{"key":"Source"}"#)]);
    patch_uncompressed_size(&mut nested, path.as_bytes(), 5 * 1024 * 1024);
    let jar = temporary_jar(&[("META-INF/jars/inner.jar", &nested)]);
    let result = scan_mod_jar(&jar.0, &ScanOptions::default());
    assert_eq!(result.source_key_count, 0);
    assert!(matches!(result.status, ModLanguageStatus::Failed { .. }));
}

#[test]
fn lightweight_scan_discovers_metadata_declared_nested_languages() {
    let nested = make_jar(&[("assets/example/lang/en_us.json", br#"{"key":"Source"}"#)]);
    let jar = temporary_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"example","version":"1","jars":[{"file":"lib/inner.jar"}]}"#,
        ),
        ("lib/inner.jar", &nested),
    ]);
    let result = scan_mod_jar(&jar.0, &ScanOptions::default());
    assert_eq!(result.source_key_count, 1);
    assert_eq!(result.status, ModLanguageStatus::Untranslated);
}

#[test]
fn lightweight_folder_scan_returns_cancelled_before_reading_the_folder() {
    let directory = temporary_directory();
    let cancellation = CancellationToken::default();
    cancellation.cancel();

    let error = scan_mods_folder(&directory.0, &ScanOptions::default(), &cancellation, |_| {})
        .expect_err("cancelled scan must stop");

    assert!(matches!(error, AnalysisError::Cancelled));
}

#[test]
fn lightweight_folder_scan_ignores_non_jars_and_subfolders_and_sorts_results() {
    let directory = temporary_directory();
    let subdirectory = directory.0.join("subfolder");
    fs::create_dir_all(&subdirectory).expect("create nested fixture directory");
    let jar_bytes = make_jar(&[("assets/mod/lang/en_us.json", br#"{"key":"value"}"#)]);
    fs::write(directory.0.join("zeta.jar"), &jar_bytes).expect("write lower-case JAR");
    fs::write(directory.0.join("Alpha.JAR"), &jar_bytes).expect("write upper-case JAR");
    fs::write(directory.0.join("ignored.jar.disabled"), &jar_bytes).expect("write disabled file");
    fs::write(directory.0.join("ignored.zip"), &jar_bytes).expect("write ZIP file");
    fs::write(subdirectory.join("nested.jar"), &jar_bytes).expect("write nested JAR");

    let progress = Mutex::new(Vec::new());
    let results = scan_mods_folder(
        &directory.0,
        &ScanOptions::default(),
        &CancellationToken::default(),
        |item| progress.lock().expect("lock progress").push(item),
    )
    .expect("scan mods folder");

    assert_eq!(
        results
            .iter()
            .map(|result| result.file_name.as_str())
            .collect::<Vec<_>>(),
        ["Alpha.JAR", "zeta.jar"]
    );
    let progress = progress.into_inner().expect("take progress");
    assert_eq!(progress.len(), 2);
    assert_eq!(progress.last().map(|item| item.completed), Some(2));
}
