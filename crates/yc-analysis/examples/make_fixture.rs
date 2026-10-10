use std::{
    env,
    error::Error,
    fs::File,
    io::{Cursor, Write},
    path::PathBuf,
};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let output = args.next().map(PathBuf::from).ok_or(
        "usage: cargo run -p yc-analysis --example make_fixture -- <output.jar> [--keys count]",
    )?;
    let mut requested_key_count = None;
    while let Some(argument) = args.next() {
        if argument != "--keys" {
            return Err("usage: cargo run -p yc-analysis --example make_fixture -- <output.jar> [--keys count]".into());
        }
        let key_count = args
            .next()
            .ok_or("--keys requires a count")?
            .to_string_lossy()
            .parse::<usize>()?;
        if key_count < 4 {
            return Err("--keys must be at least 4 to retain the base fixture entries".into());
        }
        requested_key_count = Some(key_count);
    }
    let nested = make_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"sample_library","name":"Sample Library","version":"1.0"}"#,
        ),
        (
            "assets/sample_library/lang/en_us.json",
            br#"{"library.message":"Library message"}"#,
        ),
    ])?;
    // 一つの既存キーはsample_tools由来でeditor一覧に含まれるため、500件指定なら残りをsampleへ作る。
    let source_key_count = requested_key_count.map_or(3, |key_count| key_count - 1);
    let mut source_language = String::from(
        r#"{"block.sample.machine":"Machine","item.sample.wrench":"Wrench","message.sample.owner":"Hello, %s!"}"#,
    );
    if source_key_count > 3 {
        source_language.pop();
        for key_index in 3..source_key_count {
            source_language.push_str(&format!(
                ",\"message.sample.scroll_{key_index:04}\":\"Scroll row {key_index}\""
            ));
        }
        source_language.push('}');
    }
    let jar = make_jar(&[
        (
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"sample","name":"Analysis Fixture","version":"1.0.0","depends":{"minecraft":">=1.20"},"jars":[{"file":"META-INF/jars/sample-library.jar"}]}"#,
        ),
        (
            "assets/sample/lang/en_us.json",
            source_language.as_bytes(),
        ),
        (
            "assets/sample/lang/ja_jp.json",
            r#"{"block.sample.machine":"機械"}"#.as_bytes(),
        ),
        (
            "assets/sample/lang/zh_cn.json",
            r#"{"block.sample.machine":"机器","item.sample.wrench":"扳手","message.sample.owner":"你好，%s！"}"#.as_bytes(),
        ),
        (
            "assets/sample_tools/lang/en_us.json",
            br#"{"item.sample_tools.hammer":"Hammer"}"#,
        ),
        (
            "assets/sample_tools/lang/ja_jp.json",
            r#"{"item.sample_tools.hammer":"ハンマー"}"#.as_bytes(),
        ),
        ("META-INF/jars/sample-library.jar", nested.as_slice()),
    ])?;
    let mut file = File::create(output)?;
    file.write_all(&jar)?;
    Ok(())
}

fn make_jar(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, contents) in entries {
        writer.start_file(
            *path,
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )?;
        writer.write_all(contents)?;
    }
    Ok(writer.finish()?.into_inner())
}
