use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    ArchiveLookup, IconMediaType, LoaderKind, analyze_metadata, find_mod_icon, parse_json_relaxed,
};

const TEST_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

#[derive(Default)]
struct TestArchive {
    files: BTreeMap<String, Vec<u8>>,
}

impl TestArchive {
    fn with(mut self, path: &str, contents: &str) -> Self {
        self.files
            .insert(path.to_owned(), contents.as_bytes().to_vec());
        self
    }

    fn with_bytes(mut self, path: &str, contents: &[u8]) -> Self {
        self.files.insert(path.to_owned(), contents.to_vec());
        self
    }
}

impl ArchiveLookup for TestArchive {
    fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }
}

#[test]
fn parses_fabric_metadata_and_nested_jars() {
    let archive = TestArchive::default().with(
        "fabric.mod.json",
        r#"{
            "id": "sample_fabric",
            "name": "Sample Fabric",
            "version": "1.2.3",
            "depends": { "minecraft": ">=1.20.1" },
            "jars": [{ "file": "META-INF/jars/helper.jar" }]
        }"#,
    );

    let result = analyze_metadata(&archive);
    let mod_info = &result.mods[0];
    assert_eq!(mod_info.loader, LoaderKind::Fabric);
    assert_eq!(mod_info.mod_id.as_deref(), Some("sample_fabric"));
    assert_eq!(mod_info.name.as_deref(), Some("Sample Fabric"));
    assert_eq!(mod_info.minecraft_version.as_deref(), Some(">=1.20.1"));
    assert_eq!(
        mod_info.minecraft_version_estimate.as_deref(),
        Some("1.20.1")
    );
    assert_eq!(result.nested_jars, ["META-INF/jars/helper.jar"]);
}

#[test]
fn parses_quilt_metadata_and_loader_nested_jars() {
    let archive = TestArchive::default().with(
        "quilt.mod.json",
        r#"{
            "quilt_loader": {
                "id": "sample_quilt",
                "version": "2.0",
                "metadata": { "name": "Sample Quilt" },
                "depends": [{ "id": "minecraft", "versions": "~1.21" }],
                "jars": [{ "file": "META-INF/jars/quilt-helper.jar" }]
            }
        }"#,
    );

    let result = analyze_metadata(&archive);
    let mod_info = &result.mods[0];
    assert_eq!(mod_info.loader, LoaderKind::Quilt);
    assert_eq!(mod_info.mod_id.as_deref(), Some("sample_quilt"));
    assert_eq!(mod_info.minecraft_version_estimate.as_deref(), Some("1.21"));
    assert_eq!(result.nested_jars, ["META-INF/jars/quilt-helper.jar"]);
}

#[test]
fn parses_forge_multiple_mods_and_minecraft_dependencies() {
    let archive = TestArchive::default().with(
        "META-INF/mods.toml",
        r#"
            [[mods]]
            modId = "first"
            version = "1.0"
            displayName = "First"

            [[mods]]
            modId = "second"
            version = "2.0"
            displayName = "Second"

            [[dependencies.first]]
            modId = "minecraft"
            versionRange = "[1.20.1,1.21)"
            mandatory = true

            [[dependencies.second]]
            modId = "minecraft"
            versionRange = ">=1.19.4"
            mandatory = true
        "#,
    );

    let result = analyze_metadata(&archive);
    assert_eq!(result.mods.len(), 2);
    assert_eq!(result.mods[0].loader, LoaderKind::Forge);
    assert_eq!(result.mods[0].mod_id.as_deref(), Some("first"));
    assert_eq!(
        result.mods[0].minecraft_version.as_deref(),
        Some("[1.20.1,1.21)")
    );
    assert_eq!(
        result.mods[0].minecraft_version_estimate.as_deref(),
        Some("1.20.1")
    );
    assert_eq!(result.mods[1].mod_id.as_deref(), Some("second"));
    assert_eq!(
        result.mods[1].minecraft_version_estimate.as_deref(),
        Some("1.19.4")
    );
}

#[test]
fn parses_neoforge_metadata() {
    let archive = TestArchive::default().with(
        "META-INF/neoforge.mods.toml",
        r#"
            [[mods]]
            modId = "sample_neo"
            version = "1.0.0"
            displayName = "Sample NeoForge"

            [[dependencies.sample_neo]]
            modId = "minecraft"
            versionRange = "[1.21,1.22)"
        "#,
    );

    let result = analyze_metadata(&archive);
    assert_eq!(result.mods[0].loader, LoaderKind::NeoForge);
    assert_eq!(
        result.mods[0].minecraft_version.as_deref(),
        Some("[1.21,1.22)")
    );
}

#[test]
fn parses_legacy_forge_multiple_mods() {
    let archive = TestArchive::default().with(
        "mcmod.info",
        r#"[
            { "modid": "legacy_one", "name": "Legacy One", "version": "1", "mcversion": "1.12.2" },
            { "modid": "legacy_two", "name": "Legacy Two", "version": "2", "mcversion": "1.7.10" }
        ]"#,
    );

    let result = analyze_metadata(&archive);
    assert_eq!(result.mods.len(), 2);
    assert!(
        result
            .mods
            .iter()
            .all(|mod_info| mod_info.loader == LoaderKind::LegacyForge)
    );
    assert_eq!(
        result.mods[1].minecraft_version_estimate.as_deref(),
        Some("1.7.10")
    );
}

#[test]
fn estimates_supported_minecraft_range_forms_without_rewriting_them() {
    for (range, expected) in [
        (">=1.20.1", Some("1.20.1")),
        ("~1.21", Some("1.21")),
        ("[1.20.1,1.21)", Some("1.20.1")),
        ("(1.20.1,1.21)", None),
        ("<1.20.1", None),
        ("*", None),
    ] {
        let metadata = serde_json::json!({
            "id": "range_test",
            "version": "1",
            "depends": { "minecraft": range }
        })
        .to_string();
        let archive = TestArchive::default().with("fabric.mod.json", &metadata);
        let result = analyze_metadata(&archive);
        assert_eq!(result.mods[0].minecraft_version.as_deref(), Some(range));
        assert_eq!(
            result.mods[0].minecraft_version_estimate.as_deref(),
            expected
        );
    }
}

#[test]
fn substitutes_manifest_jar_version_placeholder() {
    let archive = TestArchive::default()
        .with(
            "fabric.mod.json",
            r#"{"id":"sample","version":"${file.jarVersion}"}"#,
        )
        .with(
            "META-INF/MANIFEST.MF",
            "Manifest-Version: 1.0\r\nImplementation-Version: 4.5.6\r\n\r\n",
        );

    let result = analyze_metadata(&archive);
    assert_eq!(result.mods[0].version.as_deref(), Some("4.5.6"));
}

#[test]
fn parses_bom_comments_trailing_commas_and_raw_newlines_in_json_strings() {
    // 改行をそのまま含むmetadataはstrict JSONでは読めないため、緩和処理の結果を確認する。
    let metadata = "\u{feff}{\n  // user comment\n  \"id\": \"odd_mod\",\n  \"name\": \"Odd\nMod\",\n  \"jars\": [{ \"file\": \"META-INF/jars/odd.jar\", }],\n}";
    let archive = TestArchive::default().with("fabric.mod.json", metadata);

    let result = analyze_metadata(&archive);
    assert!(result.warnings.is_empty());
    assert_eq!(result.mods[0].name.as_deref(), Some("Odd\nMod"));
    assert_eq!(result.nested_jars, ["META-INF/jars/odd.jar"]);
}

#[test]
fn malformed_metadata_warns_and_does_not_stop_other_formats() {
    let archive = TestArchive::default()
        .with("fabric.mod.json", "{ broken json")
        .with(
            "META-INF/neoforge.mods.toml",
            "[[mods]]\nmodId = \"still_found\"\nversion = \"1\"\n",
        );

    let result = analyze_metadata(&archive);
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings[0].path, "fabric.mod.json");
    assert_eq!(result.mods.len(), 1);
    assert_eq!(result.mods[0].mod_id.as_deref(), Some("still_found"));
    assert_eq!(result.mods[0].loader, LoaderKind::NeoForge);
}

#[test]
fn fallback_uses_manifest_and_discovers_all_asset_namespaces() {
    let archive = TestArchive::default()
        .with(
            "META-INF/MANIFEST.MF",
            "Implementation-Title: Shared Library\r\nImplementation-Version: 3.2\r\n\r\n",
        )
        .with("assets/alpha/lang/en_us.json", "{}")
        .with("assets/beta/textures/icon.png", "placeholder");

    let result = analyze_metadata(&archive);
    assert_eq!(result.namespaces, ["alpha", "beta"]);
    assert_eq!(result.mods.len(), 2);
    assert!(
        result
            .mods
            .iter()
            .all(|mod_info| mod_info.loader == LoaderKind::Unknown)
    );
    assert!(
        result
            .mods
            .iter()
            .all(|mod_info| mod_info.name.as_deref() == Some("Shared Library"))
    );
}

#[test]
fn parses_forge_jarjar_metadata_and_common_nested_jar_paths() {
    let archive = TestArchive::default()
        .with(
            "META-INF/jarjar/metadata.json",
            r#"{"jars":[{"path":"META-INF/jarjar/dep.jar"}]}"#,
        )
        .with("META-INF/jars/also-dep.jar", "placeholder")
        .with("META-INF/jars/deep/nested.jar", "placeholder");

    let result = analyze_metadata(&archive);
    assert_eq!(
        result.nested_jars,
        ["META-INF/jarjar/dep.jar", "META-INF/jars/also-dep.jar"]
    );
}

#[test]
fn empty_archive_returns_unknown_loader() {
    let result = analyze_metadata(&TestArchive::default());
    assert_eq!(result.mods.len(), 1);
    assert_eq!(result.mods[0].loader, LoaderKind::Unknown);
    assert!(result.mods[0].metadata_source.is_none());
    assert!(result.warnings.is_empty());
}

#[test]
fn serialized_public_fields_use_camel_case_and_loader_values_match_plan() {
    let archive = TestArchive::default().with(
        "META-INF/neoforge.mods.toml",
        "[[mods]]\nmodId = \"sample\"\nversion = \"1\"\n",
    );
    let result = analyze_metadata(&archive);
    let value: Value = serde_json::to_value(&result).unwrap();
    assert!(value["mods"][0].get("modId").is_some());
    assert!(value["mods"][0].get("metadataSource").is_some());
    assert_eq!(value["mods"][0]["loader"], "neoforge");
}

#[test]
fn quilt_loader_can_read_string_entries_in_nested_jar_list() {
    let archive = TestArchive::default().with(
        "quilt.mod.json",
        r#"{"quilt_loader":{"id":"sample","jars":["META-INF/jars/direct.jar"]}}"#,
    );
    let result = analyze_metadata(&archive);
    assert_eq!(result.nested_jars, ["META-INF/jars/direct.jar"]);
}

#[test]
fn relaxed_json_removes_comments_without_changing_comment_markers_in_strings() {
    let value =
        parse_json_relaxed(br#"{"url":"https://example.test/a//b", /* note */ "value": 1,}"#)
            .unwrap();
    assert_eq!(value["url"], "https://example.test/a//b");
    assert_eq!(value["value"], 1);
}

#[test]
fn a_manifest_without_mod_metadata_still_produces_unknown_loader() {
    let archive = TestArchive::default().with_bytes(
        "META-INF/MANIFEST.MF",
        b"Implementation-Version: 1.0\r\n\r\n",
    );
    let result = analyze_metadata(&archive);
    assert_eq!(result.mods[0].loader, LoaderKind::Unknown);
    assert_eq!(result.mods[0].version.as_deref(), Some("1.0"));
}

#[test]
fn finds_fabric_icon_from_string_and_removes_leading_slash() {
    let archive = TestArchive::default()
        .with(
            "fabric.mod.json",
            r#"{"id":"sample","icon":"/assets/sample/icon.png"}"#,
        )
        .with_bytes("assets/sample/icon.png", TEST_PNG);

    let icon = find_mod_icon(&archive, Some("sample")).unwrap();
    assert_eq!(icon.archive_path, "assets/sample/icon.png");
    assert_eq!(icon.bytes, TEST_PNG);
    assert_eq!(icon.media_type, IconMediaType::Png);
}

#[test]
fn finds_fabric_icon_from_size_map_using_smallest_size_at_least_64() {
    let archive = TestArchive::default()
        .with(
            "fabric.mod.json",
            r#"{"id":"sample","icon":{"32":"icon-32.png","128":"icon-128.png","64":"icon-64.png"}}"#,
        )
        .with_bytes("icon-32.png", TEST_PNG)
        .with_bytes("icon-64.png", TEST_PNG)
        .with_bytes("icon-128.png", TEST_PNG);

    let icon = find_mod_icon(&archive, None).unwrap();
    assert_eq!(icon.archive_path, "icon-64.png");
}

#[test]
fn size_map_without_a_size_at_least_64_uses_its_largest_entry() {
    let archive = TestArchive::default()
        .with(
            "fabric.mod.json",
            r#"{"id":"sample","icon":{"16":"small.png","48":"large.png"}}"#,
        )
        .with_bytes("small.png", TEST_PNG)
        .with_bytes("large.png", TEST_PNG);

    let icon = find_mod_icon(&archive, None).unwrap();
    assert_eq!(icon.archive_path, "large.png");
}

#[test]
fn finds_quilt_icon_from_loader_metadata() {
    let archive = TestArchive::default()
        .with(
            "quilt.mod.json",
            r#"{"quilt_loader":{"id":"sample","metadata":{"icon":"icon.png"}}}"#,
        )
        .with_bytes("icon.png", TEST_PNG);

    assert_eq!(
        find_mod_icon(&archive, Some("sample"))
            .unwrap()
            .archive_path,
        "icon.png"
    );
}

#[test]
fn selects_mods_toml_logo_matching_mod_id() {
    let archive = TestArchive::default()
        .with(
            "META-INF/mods.toml",
            "[[mods]]\nmodId = \"first\"\nlogoFile = \"first.png\"\n\n[[mods]]\nmodId = \"selected\"\nlogoFile = \"selected.png\"\n",
        )
        .with_bytes("first.png", TEST_PNG)
        .with_bytes("selected.png", TEST_PNG);

    assert_eq!(
        find_mod_icon(&archive, Some("selected"))
            .unwrap()
            .archive_path,
        "selected.png"
    );
}

#[test]
fn uses_top_level_toml_logo_when_mods_have_no_logo() {
    let archive = TestArchive::default()
        .with(
            "META-INF/neoforge.mods.toml",
            "logoFile = \"top.png\"\n\n[[mods]]\nmodId = \"sample\"\n",
        )
        .with_bytes("top.png", TEST_PNG);

    assert_eq!(
        find_mod_icon(&archive, Some("sample"))
            .unwrap()
            .archive_path,
        "top.png"
    );
}

#[test]
fn finds_legacy_forge_logo_and_pack_png_fallback() {
    let legacy = TestArchive::default()
        .with(
            "mcmod.info",
            r#"[{"modid":"sample","logoFile":"legacy.png"}]"#,
        )
        .with_bytes("legacy.png", TEST_PNG);
    assert_eq!(
        find_mod_icon(&legacy, Some("sample")).unwrap().archive_path,
        "legacy.png"
    );

    let pack = TestArchive::default().with_bytes("pack.png", TEST_PNG);
    assert_eq!(find_mod_icon(&pack, None).unwrap().archive_path, "pack.png");
}

#[test]
fn skips_broken_and_non_png_icons_and_tries_the_next_metadata_format() {
    let archive = TestArchive::default()
        .with("fabric.mod.json", r#"{"id":"sample","icon":"broken.png"}"#)
        .with(
            "quilt.mod.json",
            r#"{"quilt_loader":{"id":"sample","metadata":{"icon":"icon.svg"}}}"#,
        )
        .with(
            "META-INF/mods.toml",
            "[[mods]]\nmodId = \"sample\"\nlogoFile = \"icon.png\"\n",
        )
        .with_bytes("broken.png", b"\x89PNG\r\n\x1a\nbroken")
        .with_bytes("icon.svg", b"<svg></svg>")
        .with_bytes("icon.png", TEST_PNG);

    assert_eq!(
        find_mod_icon(&archive, Some("sample"))
            .unwrap()
            .archive_path,
        "icon.png"
    );
}

#[test]
fn rejects_parent_traversal_and_uses_pack_png() {
    let archive = TestArchive::default()
        .with(
            "fabric.mod.json",
            r#"{"id":"sample","icon":"../outside.png"}"#,
        )
        .with_bytes("outside.png", TEST_PNG)
        .with_bytes("pack.png", TEST_PNG);

    assert_eq!(
        find_mod_icon(&archive, Some("sample"))
            .unwrap()
            .archive_path,
        "pack.png"
    );
}

#[test]
fn ignores_icons_larger_than_one_mib() {
    let mut oversized = TEST_PNG.to_vec();
    oversized.resize(1024 * 1024 + 1, 0);
    let archive = TestArchive::default()
        .with("fabric.mod.json", r#"{"id":"sample","icon":"large.png"}"#)
        .with_bytes("large.png", &oversized)
        .with_bytes("pack.png", TEST_PNG);

    assert_eq!(
        find_mod_icon(&archive, Some("sample"))
            .unwrap()
            .archive_path,
        "pack.png"
    );
}
