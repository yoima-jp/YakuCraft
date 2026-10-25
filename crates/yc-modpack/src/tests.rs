use std::collections::BTreeMap;

use crate::{
    FtbQuestsStatus, SnbtValue, analyze_files, parse_snbt, scan_instance, write_ja_jp, write_snbt,
};

fn files(items: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    items
        .iter()
        .map(|(path, contents)| (path.to_string(), contents.as_bytes().to_vec()))
        .collect()
}

#[test]
fn snbt_round_trip_preserves_arrays_empty_lines_escapes_and_key_order() {
    let input = r#"{
        # comments are allowed before keys
        quest.1.title: "A \"quote\" and a \\ slash"
        quest.1.quest_desc: [
            "line one\\nline two"
            "raw escape \n remains literal"
            ""
            "comma, and colon: stay in the value",
            'single quoted input'
        ]
    }"#;

    let parsed = parse_snbt(input).expect("SNBT should parse");
    assert_eq!(parsed.entries[0].0, "quest.1.title");
    let SnbtValue::StringArray(description) = &parsed.entries[1].1 else {
        panic!("description must stay an array");
    };
    assert_eq!(description.len(), 5);
    assert_eq!(description[1], "raw escape \\n remains literal");
    assert_eq!(description[2], "");
    assert_eq!(description[0], "line one\\nline two");
    assert_eq!(parse_snbt(&write_snbt(&parsed)).unwrap(), parsed);
}

#[test]
fn single_file_prefers_existing_ja_values_and_keeps_nontranslatable_lines() {
    let imported = files(&[
        (
            "config/ftbquests/quests/lang/en_us.snbt",
            r#"{
                # FTB language comments
                quest.q.title: "English title"
                quest.q.quest_desc: [
                    "First paragraph"
                    ""
                    "{@pagebreak}"
                    "{image:example:icon}"
                    "Last \\\\ path and \"quoted\""
                ]
            }"#,
        ),
        (
            "config/ftbquests/quests/lang/ja_jp.snbt",
            r#"{
                quest.q.title: "既存タイトル"
                quest.q.quest_desc: ["一段落", ""]
            }"#,
        ),
    ]);

    let data = analyze_files(&imported).expect("modpack language files should parse");
    assert_eq!(data.scan.ftb_quests, FtbQuestsStatus::Single);
    assert_eq!(data.entries.len(), 6);
    assert!(!data.entries[2].translatable);
    assert!(!data.entries[3].translatable);
    assert!(!data.entries[4].translatable);
    assert_eq!(
        data.entries[0].existing_translation.as_deref(),
        Some("既存タイトル")
    );

    let translations = BTreeMap::from([(
        "ftbquests:en_us.snbt#quest.q.title".to_owned(),
        "翻訳タイトル".to_owned(),
    )]);
    let output = write_ja_jp(&data, &translations).expect("Japanese FTB output should be written");
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].path, "config/ftbquests/quests/lang/ja_jp.snbt");
    let rendered = std::str::from_utf8(&output[0].bytes).unwrap();
    let parsed = parse_snbt(rendered).unwrap();
    let values = parsed.entries;
    assert_eq!(values[0].1, SnbtValue::String("翻訳タイトル".into()));
    let SnbtValue::StringArray(description) = &values[1].1 else {
        panic!("description must remain an array");
    };
    assert_eq!(description.len(), 5);
    assert_eq!(description[0], "一段落");
    assert_eq!(description[1], "");
    assert_eq!(description[2], "{@pagebreak}");
    assert_eq!(description[3], "{image:example:icon}");
    assert_eq!(description[4], "Last \\\\ path and \"quoted\"");
}

#[test]
fn split_ftb_files_override_a_stale_single_file() {
    let imported = files(&[
        (
            "minecraft/config/ftbquests/quests/lang/en_us.snbt",
            "{ quest.q.title: \"stale single\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/en_us/chapters/start.snbt",
            "{ quest.q.title: \"canonical split\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/ja_jp.snbt",
            "{ quest.q.title: \"stale Japanese\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/ja_jp/chapters/start.snbt",
            "{ quest.q.title: \"既存\" }",
        ),
    ]);

    let data = analyze_files(&imported).expect("split language files should parse");
    assert_eq!(data.scan.ftb_quests, FtbQuestsStatus::Split);
    assert_eq!(data.entries.len(), 1);
    assert_eq!(data.entries[0].source_text, "canonical split");
    assert_eq!(
        data.entries[0].existing_translation.as_deref(),
        Some("既存")
    );
    assert_eq!(
        data.entries[0].key,
        "ftbquests:chapters/start.snbt#quest.q.title"
    );

    let output = write_ja_jp(&data, &BTreeMap::new()).unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(
        output[0].path,
        "minecraft/config/ftbquests/quests/lang/ja_jp/chapters/start.snbt"
    );
    let parsed = parse_snbt(std::str::from_utf8(&output[0].bytes).unwrap()).unwrap();
    assert_eq!(parsed.entries[0].1, SnbtValue::String("既存".into()));
    assert_eq!(data.scan.game_directory, "minecraft");
}

#[test]
fn blank_ftb_translation_uses_the_existing_translation_as_untranslated_does() {
    let imported = files(&[
        (
            "config/ftbquests/quests/lang/en_us.snbt",
            "{ quest.q.title: \"English title\" }",
        ),
        (
            "config/ftbquests/quests/lang/ja_jp.snbt",
            "{ quest.q.title: \"既存タイトル\" }",
        ),
    ]);
    let data = analyze_files(&imported).expect("FTB language files should parse");
    let translations = BTreeMap::from([(
        "ftbquests:en_us.snbt#quest.q.title".to_owned(),
        " \u{3000}".to_owned(),
    )]);

    let output = write_ja_jp(&data, &translations).expect("FTB output should be written");
    let parsed = parse_snbt(std::str::from_utf8(&output[0].bytes).unwrap()).unwrap();
    assert_eq!(
        parsed.entries[0].1,
        SnbtValue::String("既存タイトル".to_owned())
    );
}

#[test]
fn old_inline_chapters_are_reported_without_extracting_entries() {
    let imported = files(&[(
        "config/ftbquests/quests/chapters/old.snbt",
        "{ quests: [{ title: \"inline quest text\" }] }",
    )]);

    let data = analyze_files(&imported).expect("legacy state detection does not parse chapters");
    assert_eq!(data.scan.ftb_quests, FtbQuestsStatus::LegacyInline);
    assert!(data.entries.is_empty());
}

#[test]
fn kubejs_namespaces_are_extracted_and_written_in_instance_relative_paths() {
    let imported = files(&[
        (
            ".minecraft/kubejs/assets/example/lang/en_us.json",
            r#"{"item.example.widget":"Widget","tooltip.example":"Tooltip"}"#,
        ),
        (
            ".minecraft/kubejs/assets/example/lang/ja_jp.json",
            r#"{"item.example.widget":"ウィジェット"}"#,
        ),
    ]);

    let data = analyze_files(&imported).expect("KubeJS JSON language files should parse");
    assert_eq!(data.scan.kubejs_namespaces, ["example"]);
    assert_eq!(data.entries.len(), 2);
    assert_eq!(
        data.entries[0].existing_translation.as_deref(),
        Some("ウィジェット")
    );
    let translations = BTreeMap::from([(
        "kubejs:example#tooltip.example".to_owned(),
        "ツールチップ".to_owned(),
    )]);
    let output = write_ja_jp(&data, &translations).unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(
        output[0].path,
        ".minecraft/kubejs/assets/example/lang/ja_jp.json"
    );
    let written: BTreeMap<String, String> = serde_json::from_slice(&output[0].bytes).unwrap();
    assert_eq!(written["item.example.widget"], "ウィジェット");
    assert_eq!(written["tooltip.example"], "ツールチップ");
}

#[test]
fn scan_instance_resolves_launcher_game_subdirectories() {
    let root = std::env::temp_dir().join(format!("yc-modpack-test-{}", std::process::id()));
    let game = root.join("minecraft");
    std::fs::create_dir_all(game.join("config/ftbquests/quests/lang")).unwrap();
    std::fs::write(
        game.join("config/ftbquests/quests/lang/en_us.snbt"),
        "{ quest.q.title: \"Title\" }",
    )
    .unwrap();
    let scan = scan_instance(&root);
    assert_eq!(scan.game_directory, "minecraft");
    assert_eq!(scan.ftb_quests, FtbQuestsStatus::Single);
    assert_eq!(scan.total_entries, 1);
    std::fs::remove_dir_all(root).unwrap();
}
