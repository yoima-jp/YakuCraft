use super::*;

#[test]
fn properties_and_round_trip() {
    let parsed = parse_lang("# comment\n! comment\na:あ\\\n  い\n".as_bytes());
    assert_eq!(parsed.entries, vec![("a".into(), "あい".into())]);
    let entries = vec![("key".into(), " 日本語\\\n次".into())];
    assert_eq!(parse_lang(&write_lang(&entries)).entries, entries);
}

#[test]
fn writer_keeps_shared_value_characters_raw_for_both_loaders() {
    let entries = vec![(
        "shader.option".to_owned(),
        "等号=コロン: タブ\t§a # !".to_owned(),
    )];
    let bytes = write_lang(&entries);
    let optifine_entries: Vec<_> = String::from_utf8(bytes.clone())
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

    assert_eq!(optifine_entries, entries);
    assert_eq!(parse_lang(&bytes).entries, entries);
}

#[test]
fn parsed_lang_retains_the_raw_key_for_export() {
    let parsed = parse_lang(b"shader\\:option=old\n");

    assert_eq!(parsed.entries, vec![("shader:option".into(), "old".into())]);
    assert_eq!(
        parsed.raw_keys.get("shader:option"),
        Some(&"shader\\:option".to_owned())
    );
    assert_eq!(
        write_lang_with_raw_keys(
            &[("shader:option".into(), "translated".into())],
            &parsed.raw_keys
        ),
        b"shader\\:option=translated\n"
    );

    let spaced = parse_lang(b"shader.option =old\n");
    assert_eq!(
        spaced.raw_keys.get("shader.option"),
        Some(&"shader.option ".to_owned())
    );
    assert_eq!(
        write_lang_with_raw_keys(
            &[("shader.option".into(), "translated".into())],
            &spaced.raw_keys
        ),
        b"shader.option =translated\n"
    );

    let continued = parse_lang(b"part\\\n  key\\:sub=old\n");
    assert_eq!(
        continued.entries,
        vec![("partkey:sub".into(), "old".into())]
    );
    assert!(!continued.raw_keys.contains_key("partkey:sub"));
    assert_eq!(
        write_lang_with_raw_keys(
            &[("partkey:sub".into(), "translated".into())],
            &continued.raw_keys
        ),
        b"partkey\\:sub=translated\n"
    );
}

#[test]
fn filename_case() {
    assert_eq!(japanese_filename("en_US.lang"), "ja_JP.lang");
    assert_eq!(japanese_filename("en_us.lang"), "ja_jp.lang");
    assert_eq!(japanese_filename("english.lang"), "ja_jp.lang");
}

#[test]
fn nested_zip_replaces_both_japanese_files() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("pack.zip");
    let output = dir.path().join("output.zip");
    let mut zip = ZipWriter::new(File::create(&source)?);
    for (name, contents) in [
        (
            "pack-main/shaders/lang/en_US.lang",
            &b"shader\\:option=old"[..],
        ),
        ("pack-main/shaders/lang/ja_JP.lang", &b"old"[..]),
        ("pack-main/shaders/lang/ja_jp.lang", &b"old"[..]),
        ("pack-main/shaders/main.fsh", &b"old"[..]),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(contents)?;
    }
    zip.finish()?;
    assert_eq!(analyze_zip(&source)?.root, "pack-main/");
    write_translated_zip(
        &source,
        &output,
        "en_US.lang",
        &[("shader:option".into(), "あ".into())],
    )?;
    let analysis = analyze_zip(&output)?;
    assert_eq!(
        analysis
            .files
            .iter()
            .filter(|f| f.locale == "ja_jp")
            .count(),
        1
    );
    let mut zip = ZipArchive::new(File::open(output)?)?;
    let mut names = Vec::new();
    for index in 0..zip.len() {
        names.push(zip.by_index(index)?.name()?.into_owned());
    }
    assert!(names.iter().any(|name| name == "pack-main/shaders/"));
    assert!(names.iter().any(|name| name == "pack-main/shaders/lang/"));
    let copied_names: Vec<_> = names
        .iter()
        .filter(|name| {
            matches!(
                name.as_str(),
                "pack-main/shaders/lang/en_US.lang" | "pack-main/shaders/main.fsh"
            )
        })
        .map(String::as_str)
        .collect();
    assert_eq!(
        copied_names,
        [
            "pack-main/shaders/lang/en_US.lang",
            "pack-main/shaders/main.fsh"
        ]
    );
    let mut translated_bytes = Vec::new();
    {
        let mut translated = zip.by_name("pack-main/shaders/lang/ja_JP.lang")?;
        translated.read_to_end(&mut translated_bytes)?;
    }
    assert_eq!(translated_bytes, "shader\\:option=あ\n".as_bytes());
    let mut shader_bytes = Vec::new();
    {
        let mut shader = zip.by_name("pack-main/shaders/main.fsh")?;
        shader.read_to_end(&mut shader_bytes)?;
    }
    assert_eq!(shader_bytes, b"old");
    Ok(())
}
