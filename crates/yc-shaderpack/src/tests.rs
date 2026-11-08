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
    assert_eq!(japanese_filename("en_US.lang", "ja_jp"), "ja_JP.lang");
    assert_eq!(japanese_filename("en_us.lang", "ja_jp"), "ja_jp.lang");
    assert_eq!(japanese_filename("english.lang", "ja_jp"), "ja_jp.lang");
    assert_eq!(japanese_filename("en_US.lang", "de_de"), "de_DE.lang");
    assert_eq!(japanese_filename("en_US.lang", "tok"), "tok.lang");
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
        "ja_jp",
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

#[test]
fn japanese_shaderpack_output_keeps_source_locale_case_with_existing_target() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("pack.zip");
    let output = dir.path().join("output.zip");
    let mut zip = ZipWriter::new(File::create(&source)?);
    for (name, contents) in [
        (
            "pack-main/shaders/lang/en_US.lang",
            &b"shader\\:option=old"[..],
        ),
        ("pack-main/shaders/lang/ja_jp.lang", &b"old"[..]),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(contents)?;
    }
    zip.finish()?;

    write_translated_zip(
        &source,
        &output,
        "en_US.lang",
        "ja_jp",
        &[("shader:option".into(), "あ".into())],
    )?;
    let mut zip = ZipArchive::new(File::open(output)?)?;
    assert!(zip.by_name("pack-main/shaders/lang/ja_JP.lang").is_ok());
    assert!(zip.by_name("pack-main/shaders/lang/ja_jp.lang").is_err());
    Ok(())
}

#[test]
fn translated_zip_adds_extra_files_at_the_archive_root_only_when_passed() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("pack.zip");
    let without = dir.path().join("without.zip");
    let with = dir.path().join("with.zip");
    let mut zip = ZipWriter::new(File::create(&source)?);
    for (name, contents) in [
        ("shaders/lang/en_US.lang", &b"option=old"[..]),
        ("shaders/main.fsh", &b"shader"[..]),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(contents)?;
    }
    zip.finish()?;

    write_translated_zip(
        &source,
        &without,
        "en_US.lang",
        "ja_jp",
        &[("option".into(), "あ".into())],
    )?;
    write_translated_zip_with_extra_files(
        &source,
        &with,
        "en_US.lang",
        "ja_jp",
        &[("option".into(), "あ".into())],
        &[(
            "PLaMo_NOTICE.txt".to_owned(),
            b"Built with PLaMo\n".to_vec(),
        )],
    )?;

    let mut without_zip = ZipArchive::new(File::open(&without)?)?;
    assert!(without_zip.by_name("PLaMo_NOTICE.txt").is_err());
    let mut with_zip = ZipArchive::new(File::open(&with)?)?;
    let mut notice = Vec::new();
    with_zip
        .by_name("PLaMo_NOTICE.txt")?
        .read_to_end(&mut notice)?;
    assert_eq!(notice, b"Built with PLaMo\n");
    Ok(())
}

#[test]
fn german_shaderpack_uses_de_de_and_english_target_keeps_untranslated_keys() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("pack.zip");
    let german_output = dir.path().join("german.zip");
    let english_output = dir.path().join("english.zip");
    let mut zip = ZipWriter::new(File::create(&source)?);
    for (name, contents) in [
        (
            "shaders/lang/en_US.lang",
            &b"keep=Original\nchange=English\nclear=English\n"[..],
        ),
        ("shaders/lang/de_DE.lang", &b"keep=Alt\n"[..]),
        ("shaders/main.fsh", &b"shader"[..]),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(contents)?;
    }
    zip.finish()?;

    write_translated_zip(
        &source,
        &german_output,
        "en_US.lang",
        "de_de",
        &[("keep".into(), "Neu".into())],
    )?;
    let mut german_zip = ZipArchive::new(File::open(&german_output)?)?;
    let mut german_bytes = Vec::new();
    german_zip
        .by_name("shaders/lang/de_DE.lang")?
        .read_to_end(&mut german_bytes)?;
    assert_eq!(german_bytes, b"keep=Neu\n");

    write_translated_zip(
        &source,
        &english_output,
        "de_DE.lang",
        "en_us",
        &[
            ("change".into(), "Changed".into()),
            ("clear".into(), String::new()),
        ],
    )?;
    let mut english_zip = ZipArchive::new(File::open(&english_output)?)?;
    let mut english_bytes = Vec::new();
    english_zip
        .by_name("shaders/lang/en_US.lang")?
        .read_to_end(&mut english_bytes)?;
    assert_eq!(
        english_bytes,
        b"keep=Original\nchange=Changed\nclear=English\n"
    );
    Ok(())
}
