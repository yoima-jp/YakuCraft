use super::*;

#[test]
fn properties_and_round_trip() {
    let parsed = parse_lang("# comment\n! comment\na:あ\\\n  い\n".as_bytes());
    assert_eq!(parsed.entries, vec![("a".into(), "あい".into())]);
    let entries = vec![("key".into(), " 日本語\\\n次".into())];
    assert_eq!(parse_lang(&write_lang(&entries)).entries, entries);
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
    for name in [
        "pack-main/shaders/lang/en_US.lang",
        "pack-main/shaders/lang/ja_JP.lang",
        "pack-main/shaders/lang/ja_jp.lang",
        "pack-main/shaders/main.fsh",
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(b"old")?;
    }
    zip.finish()?;
    assert_eq!(analyze_zip(&source)?.root, "pack-main/");
    write_translated_zip(&source, &output, "en_US.lang", &[("a".into(), "あ".into())])?;
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
    assert!(zip.by_name("pack-main/shaders/main.fsh").is_ok());
    Ok(())
}
