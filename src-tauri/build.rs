fn main() {
    // tauri-buildはicon.icoをWindowsリソースとしてexeに埋め込むが、icons/の差し替えだけでは
    // build scriptが再実行されず、古いアイコンのexeがincremental buildで使い回されることがあった。
    // アイコンを変えたら確実に埋め込み直されるよう、ディレクトリを監視対象にする。
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build();
}
