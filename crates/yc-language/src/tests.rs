use std::cmp::Ordering;

use crate::{
    DiscoveredLanguageFile, EntryFlag, LanguageArchivePath, LanguageFileFormat,
    LanguageParseWarning, ParsedLanguageFile, ReviewSettings, TranslationEntry, TranslationOrigin,
    WriterOptions, compare_minecraft_versions, discover_language_files, language_export_filename,
    merge_languages, normalize_locale, parse_json_language, parse_language_file,
    parse_legacy_language, recommend_source_locale, resource_pack_language_filename,
    write_json_language, write_legacy_language,
};

fn parsed_json(locale: &str, container_path: &str, source: &str) -> ParsedLanguageFile {
    parsed_json_in("mod", locale, container_path, source)
}

fn parsed_json_in(
    namespace: &str,
    locale: &str,
    container_path: &str,
    source: &str,
) -> ParsedLanguageFile {
    let archive_path = format!("assets/{namespace}/lang/{locale}.json");
    let file = if container_path.is_empty() {
        LanguageArchivePath::root(archive_path)
    } else {
        LanguageArchivePath::nested(container_path, archive_path)
    };
    let discovered = discover_language_files(&[file]).remove(0);
    parse_language_file(discovered, source.as_bytes()).unwrap()
}

fn settings(source_locale: &str) -> ReviewSettings {
    ReviewSettings {
        source_locale: source_locale.to_owned(),
        included_namespaces: vec!["mod".to_owned()],
        ..ReviewSettings::default()
    }
}

fn output_entry(key: &str, text: &str, sort_index: usize) -> TranslationEntry {
    TranslationEntry {
        namespace: "mod".to_owned(),
        key: key.to_owned(),
        source_text: None,
        source_locale: None,
        target_text: text.to_owned(),
        target_original_text: None,
        translated_source_text: None,
        origin: TranslationOrigin::Missing,
        flags: Vec::new(),
        sort_index,
    }
}

fn discovered(locale: &str, extension: &str, container_path: &str) -> DiscoveredLanguageFile {
    let archive_path = format!("assets/mod/lang/{locale}.{extension}");
    let input = if container_path.is_empty() {
        LanguageArchivePath::root(archive_path)
    } else {
        LanguageArchivePath::nested(container_path, archive_path)
    };
    discover_language_files(&[input]).remove(0)
}

#[test]
fn discovers_root_and_nested_json_and_legacy_files() {
    let files = discover_language_files(&[
        LanguageArchivePath::nested("META-INF/jars/library.jar", "assets/lib/lang/zh_CN.lang"),
        LanguageArchivePath::root("assets/mod/lang/en_us.json"),
        LanguageArchivePath::root("assets/mod/text/en_us.json"),
        LanguageArchivePath::root("assets/mod/lang/readme.txt"),
        LanguageArchivePath::root("assets/../lang/en_us.json"),
    ]);

    assert_eq!(files.len(), 2);
    assert_eq!(files[0].namespace, "mod");
    assert_eq!(files[0].locale, "en_us");
    assert_eq!(files[0].container_path, "");
    assert_eq!(files[0].format, LanguageFileFormat::Json);
    assert_eq!(files[1].namespace, "lib");
    assert_eq!(files[1].locale, "zh_CN");
    assert_eq!(files[1].normalized_locale, "zh_cn");
    assert_eq!(files[1].container_path, "META-INF/jars/library.jar");
    assert_eq!(files[1].format, LanguageFileFormat::LegacyLang);
}

#[test]
fn json_parser_preserves_order_bom_malformed_values_and_last_duplicate() {
    let source = format!(
        "\u{feff}{}",
        r#"{"first":"最初","invalid":3,"second":"二番目","first":"最後"}"#
    );
    let parsed = parse_json_language(source.as_bytes()).unwrap();

    assert_eq!(
        parsed
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["first", "invalid", "second"]
    );
    assert_eq!(parsed.entries["first"].text.as_deref(), Some("最後"));
    assert!(!parsed.entries["first"].malformed_source);
    assert_eq!(parsed.entries["invalid"].text, None);
    assert!(parsed.entries["invalid"].malformed_source);
    assert_eq!(
        parsed.warnings,
        [LanguageParseWarning::DuplicateKey {
            key: "first".to_owned()
        }]
    );
}

#[test]
fn legacy_parser_keeps_vanilla_values_literal_without_parse_escapes() {
    let source = r#"
# comment

line=first\nsecond
slash=C:\mods
windows_path=C:\new
equals=a=b=c
duplicate=old
duplicate=new
"#;
    let parsed = parse_legacy_language(source.as_bytes());
    assert_eq!(
        parsed.entries["line"].text.as_deref(),
        Some(r"first\nsecond")
    );
    assert_eq!(parsed.entries["slash"].text.as_deref(), Some(r"C:\mods"));
    assert_eq!(
        parsed.entries["windows_path"].text.as_deref(),
        Some(r"C:\new")
    );
    assert_eq!(parsed.entries["equals"].text.as_deref(), Some("a=b=c"));
    assert_eq!(parsed.entries["duplicate"].text.as_deref(), Some("new"));
    assert!(!parsed.legacy_parse_escapes);
    assert_eq!(parsed.warnings.len(), 1);
    assert_eq!(
        parsed.warnings[0],
        LanguageParseWarning::DuplicateKey {
            key: "duplicate".to_owned()
        }
    );

    let mut non_utf8 = b"accent=".to_vec();
    non_utf8.push(0xE9);
    let decoded = parse_legacy_language(&non_utf8);
    assert_eq!(decoded.entries["accent"].text.as_deref(), Some("é"));
    assert_eq!(
        decoded.warnings,
        [LanguageParseWarning::EncodingFallback {
            encoding: "windows-1252".to_owned()
        }]
    );
}

#[test]
fn legacy_parse_escapes_mode_decodes_java_properties_and_round_trips_both_modes() {
    let vanilla_source = concat!("path=C:\\new\n", "slash=C:\\mods\n");
    let vanilla = parse_legacy_language(vanilla_source.as_bytes());
    assert!(!vanilla.legacy_parse_escapes);
    let vanilla_entries = [
        output_entry("path", vanilla.entries["path"].text.as_deref().unwrap(), 0),
        output_entry(
            "slash",
            vanilla.entries["slash"].text.as_deref().unwrap(),
            1,
        ),
    ];
    let vanilla_written = write_legacy_language("mod", &vanilla_entries, WriterOptions::default());
    assert!(!String::from_utf8_lossy(&vanilla_written).contains("#PARSE_ESCAPES"));
    let vanilla_round_trip = parse_legacy_language(&vanilla_written);
    assert_eq!(vanilla_round_trip.entries, vanilla.entries);

    let extended_source = concat!(
        "#PARSE_ESCAPES\n",
        "line=first\\nsecond\n",
        "path=C:\\\\new\n",
        "unicode=\\u3053\\u3093\\u306b\\u3061\\u306f\n",
        "literal=\\\\u3053\n"
    );
    let extended = parse_legacy_language(extended_source.as_bytes());
    assert!(extended.legacy_parse_escapes);
    assert_eq!(
        extended.entries["line"].text.as_deref(),
        Some("first\nsecond")
    );
    assert_eq!(extended.entries["path"].text.as_deref(), Some(r"C:\new"));
    assert_eq!(
        extended.entries["unicode"].text.as_deref(),
        Some("こんにちは")
    );
    assert_eq!(extended.entries["literal"].text.as_deref(), Some(r"\u3053"));
    let extended_entries: Vec<_> = extended
        .entries
        .iter()
        .enumerate()
        .map(|(sort_index, (key, value))| {
            output_entry(key, value.text.as_deref().unwrap(), sort_index)
        })
        .collect();
    let options = WriterOptions {
        legacy_parse_escapes: extended.legacy_parse_escapes,
        ..WriterOptions::default()
    };
    let extended_written = write_legacy_language("mod", &extended_entries, options);
    assert!(String::from_utf8_lossy(&extended_written).starts_with("#PARSE_ESCAPES\n"));
    let extended_round_trip = parse_legacy_language(&extended_written);
    assert_eq!(extended_round_trip.entries, extended.entries);
    assert!(extended_round_trip.legacy_parse_escapes);

    let marker_after_comment =
        parse_legacy_language(b"# comment\n# PARSE_ESCAPES\nvalue=\\u0041\n");
    assert!(marker_after_comment.legacy_parse_escapes);
    assert_eq!(
        marker_after_comment.entries["value"].text.as_deref(),
        Some("A")
    );

    let indented_marker = parse_legacy_language(b" #PARSE_ESCAPES\nvalue=\\n\n");
    assert!(!indented_marker.legacy_parse_escapes);
    assert_eq!(
        indented_marker.entries["value"].text.as_deref(),
        Some(r"\n")
    );
}

#[test]
fn language_export_filename_uses_an_available_locale_when_source_is_missing() {
    let fallback = discovered("fr_FR", "lang", "");
    assert_eq!(
        language_export_filename(&[fallback], "mod", "en_us").as_deref(),
        Some("ja_JP.lang")
    );
}

#[test]
fn japanese_legacy_file_uses_shift_jis_before_windows_1252() {
    let (encoded, _, had_errors) = encoding_rs::SHIFT_JIS.encode("こんにちは");
    assert!(!had_errors);
    let mut source = b"greeting=".to_vec();
    source.extend_from_slice(encoded.as_ref());

    let parsed = parse_language_file(discovered("ja_JP", "lang", ""), &source)
        .expect("Japanese legacy file should parse");

    assert_eq!(
        parsed.entries["greeting"].text.as_deref(),
        Some("こんにちは")
    );
    assert_eq!(
        parsed.warnings,
        [LanguageParseWarning::EncodingFallback {
            encoding: "shift_jis".to_owned()
        }]
    );
}

#[test]
fn locale_normalization_preserves_the_original_in_discovery_results() {
    assert_eq!(normalize_locale("ja_JP"), "ja_jp");
    let file = discovered("ja_JP", "json", "");
    assert_eq!(file.locale, "ja_JP");
    assert_eq!(file.normalized_locale, "ja_jp");
}

#[test]
fn review_defaults_match_the_preserve_and_fill_choices() {
    let defaults = ReviewSettings::default();
    assert!(defaults.preserve_existing_japanese);
    assert!(defaults.fill_missing_from_other_languages);
}

#[test]
fn merge_unions_root_and_nested_keys_with_root_values_first() {
    let files = [
        parsed_json("en_us", "", r#"{"same":"root","root":"Root"}"#),
        parsed_json(
            "en_us",
            "META-INF/jars/library.jar",
            r#"{"same":"nested","nested":"Nested"}"#,
        ),
        parsed_json(
            "zh_cn",
            "META-INF/jars/library.jar",
            r#"{"alpha":"补完","same":"翻訳"}"#,
        ),
        {
            let archive_path = "assets/ignored/lang/en_us.json";
            let file = LanguageArchivePath::root(archive_path);
            parse_language_file(
                discover_language_files(&[file]).remove(0),
                br#"{"not_included":"ignored"}"#,
            )
            .unwrap()
        },
    ];
    let mut review = settings("EN_US");
    review.fill_missing_from_other_languages = true;
    let entries = merge_languages(&files, &review);

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        ["same", "root", "nested", "alpha"]
    );
    assert_eq!(entries[0].source_text.as_deref(), Some("root"));
    assert_eq!(entries[0].source_locale.as_deref(), Some("en_us"));
    assert_eq!(entries[2].source_text.as_deref(), Some("Nested"));
    assert_eq!(entries[3].source_text.as_deref(), Some("补完"));
    assert!(entries[3].flags.contains(&EntryFlag::SourceFallback));
    assert!(entries.iter().all(|entry| entry.namespace == "mod"));
    assert!(
        entries
            .iter()
            .enumerate()
            .all(|(index, entry)| entry.sort_index == index)
    );
}

#[test]
fn malformed_primary_source_is_flagged_and_other_locale_can_fill_it() {
    let files = [
        parsed_json("en_us", "", r#"{"bad":false}"#),
        parsed_json("fr_fr", "", r#"{"bad":"補完"}"#),
    ];
    let mut review = settings("en_us");
    review.fill_missing_from_other_languages = true;
    let entry = &merge_languages(&files, &review)[0];

    assert_eq!(entry.source_text.as_deref(), Some("補完"));
    assert_eq!(entry.source_locale.as_deref(), Some("fr_fr"));
    assert!(entry.flags.contains(&EntryFlag::MalformedSource));
    assert!(entry.flags.contains(&EntryFlag::SourceFallback));
}

#[test]
fn fallback_priority_precedes_coverage_and_coverage_breaks_remaining_choices() {
    let files = [
        parsed_json("en_us", "", r#"{"english":"English"}"#),
        parsed_json("fr_fr", "", r#"{"overlap":"French","fr_only":"French"}"#),
        parsed_json(
            "de_de",
            "",
            r#"{"overlap":"German","de_only":"German","coverage":"German"}"#,
        ),
        parsed_json("ja_jp", "", r#"{"target_only":"日本語"}"#),
    ];
    let mut review = settings("en_us");
    review.fill_missing_from_other_languages = true;
    review.fallback_priority = vec!["fr_FR".to_owned()];
    let explicit = merge_languages(&files, &review);
    let overlap = explicit
        .iter()
        .find(|entry| entry.key == "overlap")
        .unwrap();
    assert_eq!(overlap.source_text.as_deref(), Some("French"));
    assert_eq!(overlap.source_locale.as_deref(), Some("fr_fr"));
    assert!(overlap.flags.contains(&EntryFlag::SourceFallback));
    let target_only = explicit
        .iter()
        .find(|entry| entry.key == "target_only")
        .unwrap();
    assert_eq!(target_only.source_text, None);
    assert!(!target_only.flags.contains(&EntryFlag::SourceFallback));

    review.fill_missing_from_other_languages = false;
    let without_fallback = merge_languages(&files, &review);
    let overlap = without_fallback
        .iter()
        .find(|entry| entry.key == "overlap")
        .unwrap();
    assert_eq!(overlap.source_text, None);
    assert!(!overlap.flags.contains(&EntryFlag::SourceFallback));

    review.fill_missing_from_other_languages = true;
    review.fallback_priority.clear();
    let by_coverage = merge_languages(&files, &review);
    let overlap = by_coverage
        .iter()
        .find(|entry| entry.key == "overlap")
        .unwrap();
    assert_eq!(overlap.source_text.as_deref(), Some("German"));
}

#[test]
fn fallback_coverage_is_scoped_to_the_current_namespace() {
    let files = [
        parsed_json("en_us", "", r#"{"source":"English"}"#),
        parsed_json("fr_fr", "", r#"{"choice":"French","local":"French"}"#),
        parsed_json("de_de", "", r#"{"choice":"German"}"#),
        parsed_json_in(
            "dependency",
            "de_de",
            "",
            r#"{"one":"1","two":"2","three":"3"}"#,
        ),
    ];
    let mut review = settings("en_us");
    review.included_namespaces.push("dependency".to_owned());
    review.fill_missing_from_other_languages = true;
    let choice = merge_languages(&files, &review)
        .into_iter()
        .find(|entry| entry.key == "choice")
        .unwrap();

    assert_eq!(choice.source_text.as_deref(), Some("French"));
}

#[test]
fn existing_japanese_is_preserved_or_started_empty_by_review_choice() {
    let files = [
        parsed_json("en_us", "", r#"{"key":"English","source_only":"Source"}"#),
        parsed_json("ja_JP", "", r#"{"key":"既存訳","target_only":"既存のみ"}"#),
    ];
    let mut review = settings("en_us");
    review.preserve_existing_japanese = true;
    let kept = merge_languages(&files, &review);
    let existing = kept.iter().find(|entry| entry.key == "key").unwrap();
    assert_eq!(existing.target_text, "既存訳");
    assert_eq!(existing.target_original_text.as_deref(), Some("既存訳"));
    assert_eq!(existing.translated_source_text.as_deref(), Some("English"));
    assert_eq!(existing.origin, TranslationOrigin::Existing);

    review.preserve_existing_japanese = false;
    let empty = merge_languages(&files, &review);
    let existing = empty.iter().find(|entry| entry.key == "key").unwrap();
    assert_eq!(existing.target_text, "");
    assert_eq!(existing.target_original_text, None);
    assert_eq!(existing.translated_source_text, None);
    assert_eq!(existing.origin, TranslationOrigin::Missing);
}

#[test]
fn source_locale_recommendation_prefers_english_then_most_entries() {
    let english_files = [
        parsed_json("en_US", "", r#"{"one":"English"}"#),
        parsed_json("de_de", "", r#"{"one":"Eins","two":"Zwei"}"#),
    ];
    assert_eq!(
        recommend_source_locale(&english_files, &["mod".to_owned()]).as_deref(),
        Some("en_US")
    );

    let no_english = [
        parsed_json("fr_fr", "", r#"{"one":"Un"}"#),
        parsed_json("de_de", "", r#"{"one":"Eins","two":"Zwei"}"#),
    ];
    assert_eq!(
        recommend_source_locale(&no_english, &["mod".to_owned()]).as_deref(),
        Some("de_de")
    );
}

#[test]
fn writers_are_deterministic_and_match_expected_bytes() {
    let entries = [
        output_entry("second", "A", 1),
        output_entry("empty", "", 2),
        output_entry("first", "一", 0),
    ];
    let options = WriterOptions::default();
    let json = write_json_language("mod", &entries, options).unwrap();
    assert_eq!(json, write_json_language("mod", &entries, options).unwrap());
    assert_eq!(
        json,
        "{\n  \"first\": \"一\",\n  \"second\": \"A\"\n}\n".as_bytes()
    );

    let with_empty = write_json_language(
        "mod",
        &entries,
        WriterOptions {
            include_empty_translations: true,
            ..WriterOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        with_empty,
        "{\n  \"first\": \"一\",\n  \"second\": \"A\",\n  \"empty\": \"\"\n}\n".as_bytes()
    );

    let legacy_entries = [
        output_entry("slash", r"C:\mods", 1),
        output_entry("line", "上\n下", 0),
    ];
    let legacy = write_legacy_language("mod", &legacy_entries, options);
    assert_eq!(
        legacy,
        write_legacy_language("mod", &legacy_entries, options)
    );
    assert_eq!(
        legacy,
        "#PARSE_ESCAPES\nline=上\\n下\nslash=C:\\\\mods\n".as_bytes()
    );
}

#[test]
fn writers_omit_whitespace_only_translations_without_trimming_translated_values() {
    let entries = [
        output_entry("ascii", " \t\r\n", 0),
        output_entry("unicode", "\u{3000}\u{00a0}", 1),
        output_entry("translated", " 翻訳 ", 2),
    ];
    let options = WriterOptions::default();
    assert_eq!(
        write_json_language("mod", &entries, options).unwrap(),
        "{\n  \"translated\": \" 翻訳 \"\n}\n".as_bytes()
    );
    assert_eq!(
        write_legacy_language("mod", &entries, options),
        "translated= 翻訳 \n".as_bytes()
    );

    let options = WriterOptions {
        include_empty_translations: true,
        ..options
    };
    let json: serde_json::Value =
        serde_json::from_slice(&write_json_language("mod", &entries, options).unwrap()).unwrap();
    assert_eq!(json["ascii"], entries[0].target_text);
    assert_eq!(json["unicode"], entries[1].target_text);
    let legacy = String::from_utf8(write_legacy_language("mod", &entries, options)).unwrap();
    let parsed = parse_legacy_language(legacy.as_bytes());
    assert_eq!(parsed.entries.len(), 3);
    assert_eq!(
        parsed.entries["ascii"].text.as_deref(),
        Some(entries[0].target_text.as_str())
    );
    assert_eq!(
        parsed.entries["unicode"].text.as_deref(),
        Some(entries[1].target_text.as_str())
    );
}

#[test]
fn language_export_filename_prefers_existing_target_then_source_style() {
    let source = discovered("en_US", "json", "");
    assert_eq!(
        language_export_filename(std::slice::from_ref(&source), "mod", "en_us").as_deref(),
        Some("ja_JP.json")
    );

    let lower_source = discovered("en_us", "lang", "");
    let existing = discovered("ja_jp", "lang", "META-INF/jars/nested.jar");
    assert_eq!(
        language_export_filename(&[existing, lower_source], "mod", "en_us").as_deref(),
        Some("ja_jp.lang")
    );
}

#[test]
fn language_export_filename_keeps_selected_source_format_in_mixed_mods() {
    for (source_format, incompatible_format, expected) in [
        ("json", "lang", "ja_JP.json"),
        ("lang", "json", "ja_JP.lang"),
    ] {
        let source = discovered("en_US", source_format, "");
        let incompatible = discovered("ja_jp", incompatible_format, "");
        assert_eq!(
            language_export_filename(&[source.clone(), incompatible.clone()], "mod", "en_us")
                .as_deref(),
            Some(expected)
        );
        assert_eq!(
            language_export_filename(&[source.clone(), incompatible.clone()], "mod", "fr_fr")
                .as_deref(),
            Some(expected)
        );
        let compatible = discovered("ja_jp", source_format, "");
        assert_eq!(
            language_export_filename(&[source, incompatible, compatible], "mod", "en_us")
                .as_deref(),
            Some(format!("ja_jp.{source_format}").as_str())
        );
    }
}

#[test]
fn resource_pack_filename_uses_numeric_minecraft_version_boundaries() {
    assert_eq!(
        resource_pack_language_filename(Some("1.10.2")).as_deref(),
        Some("ja_JP.lang")
    );
    assert_eq!(
        resource_pack_language_filename(Some("1.11")).as_deref(),
        Some("ja_jp.lang")
    );
    assert_eq!(
        resource_pack_language_filename(Some("1.12.2")).as_deref(),
        Some("ja_jp.lang")
    );
    assert_eq!(
        resource_pack_language_filename(Some("1.13")).as_deref(),
        Some("ja_jp.json")
    );
    assert_eq!(
        resource_pack_language_filename(Some("26.1")).as_deref(),
        Some("ja_jp.json")
    );
    assert_eq!(resource_pack_language_filename(None), None);
    assert_eq!(resource_pack_language_filename(Some("unknown")), None);
    assert_eq!(
        compare_minecraft_versions("1.12.2", "1.11"),
        Some(Ordering::Greater)
    );
    assert_eq!(
        compare_minecraft_versions("26.1", "1.21.9"),
        Some(Ordering::Greater)
    );
    assert_eq!(compare_minecraft_versions("unknown", "1.13"), None);
}
