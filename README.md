# YakuCraft

Windows x64 / ARM64 と Linux x86_64 向けの Minecraft Mod 翻訳アプリです。JAR やインスタンス内の Mod を調べ、日本語の言語ファイルを書き出します。

![翻訳画面](.github/assets/editor.png)

## 主な機能

- Mod JAR の翻訳データを編集し、最近のプロジェクトから再開
- Minecraft 1.20 以降の言語フォルダ型 Patchouli ガイドブックを翻訳
- FTB Quests の lang SNBT と KubeJS の言語ファイルを翻訳
- インスタンスの Mod フォルダをまとめて確認し、選んだ Mod を翻訳
- AI 翻訳、検索、翻訳状態の確認、Minecraft のブロックやアイテムのプレビュー
- リソースパック、言語ファイル、Mod JAR への書き出し

## ダウンロードとインストール

[GitHub Releases](https://github.com/yoima-jp/YakuCraft/releases) から OS に合った最新版をダウンロードしてください。

- **Windows x64**: 通常は `YakuCraft_*_x64-setup.exe` を使います。MSI も配布します。
- **Windows ARM64**: `YakuCraft_*_arm64-setup.exe` を使います。MSI も配布します。
- **Linux x86_64 (Ubuntu 22.04 以降)**: `YakuCraft_*_amd64.AppImage` または `YakuCraft_*_amd64.deb` を使います。AppImage は実行権限を付けて起動します。

ファイル名のアーキテクチャ表記は、各 OS の慣習に合わせています。Windows の `x64` と Linux の `amd64` は、どちらも Intel / AMD の 64 ビット CPU（x86_64）向けです。Snapdragon などの ARM 搭載 Windows PC では `arm64` を使ってください。

```bash
chmod +x ./*.AppImage
./*.AppImage
```

`.deb` は次のようにインストールします。`.deb` はアプリ内で自動更新されないため、新しい版が出たら新しい `.deb` をダウンロードして入れ直してください。

```bash
sudo apt install ./*.deb
```

コード署名を行っていないため、Windows SmartScreen の警告が表示されることがあります。発行元とダウンロード元を確認して実行する場合は、警告画面の「詳細情報」から「実行」を選んでください。セットアップ中に WebView2 Runtime が必要な場合は、インターネットから取得します。

## 使い方

### 1つの JAR を翻訳

ホーム画面で JAR を選ぶか、画面へドロップします。解析内容と翻訳元の言語を確認し、翻訳画面で日本語を編集します。編集内容は自動保存されます。

Minecraft 1.20 以降の Patchouli ガイドブックは、言語フォルダ型の本文を通常の翻訳項目と同じ画面で編集できます。

### インスタンス内の Mod をまとめて翻訳

ホーム画面で Minecraft インスタンスのフォルダを選び、検出された Mod を選択します。対象 Mod ごとに翻訳データを編集できます。インスタンスを検出できない場合、一部の直接配置機能は使えません。

FTB Quests は lang フォルダにある単一版・分割版の SNBT ファイルを翻訳できます。クエスト本文がチャプターファイルに直接書かれた旧形式は翻訳できません。KubeJS の `kubejs/assets/<namespace>/lang/` にある言語ファイルも翻訳できます。

インスタンス側の翻訳ファイルを更新した場合は、翻訳画面で Modpack タブのメニューから「Modpack を再取り込み」を選びます。保存済みの訳はキーで引き継ぎます。

### 書き出し

- **リソースパック**: 選んだ Mod の翻訳を ZIP にまとめます。Minecraft のリソースパック設定で有効にしてください。
- **インスタンスへ配置**: リソースパックを検出したインスタンスへ直接配置します。インスタンスを検出できない場合は選べません。
- **言語ファイル**: `assets/<namespace>/lang/` に置く言語ファイルを書き出します。Minecraft が読み込む場所へ手動で配置してください。
- **Mod JAR**: 翻訳を追加した JAR を書き出します。通常は元ファイルを変更しません。`mods` フォルダの JAR を直接置き換える方法を選ぶ場合は、Minecraft を終了してください。元の JAR はバックアップされ、アプリから戻せますが、ランチャーの更新や整合性確認で変更が戻る場合があります。
- **Modpack ファイル**: FTB Quests と KubeJS の翻訳を、インスタンス相対パスを保った ZIP に書き出すか、インスタンスへ直接配置できます。直接配置したファイルはバックアップされ、書き出し画面から復元できます。配置後に変更されたファイルは、確認してから復元します。KubeJS の言語ファイルはリソースパックにも含まれますが、FTB Quests のクエストはリソースパックでは反映できません。

空欄または空白だけの訳は未翻訳として扱います。言語ファイルと KubeJS ではそのキーを省き、Patchouli 文書では原文を残します。FTB Quests では未翻訳と同じ処理になります。

## AI 翻訳

ChatGPT、OpenAI、Claude（Anthropic）、Gemini、OpenRouter、DeepSeek、Groq、Ollama、LM Studio、OpenAI 互換（カスタム）、DeepL に対応しています。OpenAI 互換 API はプロバイダーごとの URL を設定済みです。モデル一覧を取得できる場合は候補から選ぶか、モデル ID を直接入力できます。ChatGPT はログイン後に候補を自動取得し、取得できない場合は従来の候補を表示します。Ollama と LM Studio は API キーなしで使えます。Windows では API キーと ChatGPT のログイン資格情報を Windows 資格情報マネージャーに、Linux では Secret Service に保存します。Linux で AI 翻訳を使うには、GNOME Keyring、KWallet などの Secret Service 対応ストアと D-Bus セッションが必要です。

用語集は CSV / TSV で取り込めます。列は「原文、訳語、メモ、大小区別」の順で、大小区別は `true` / `false` を指定します。メモと大小区別は省略できます。CSV の書き出しには大小区別も含まれます。

## 生成物の再配布

Mod の翻訳を含むリソースパックや JAR を配布する前に、各 Mod のライセンスと配布条件を確認してください。再配布できるかどうかは各 Mod のライセンスに従います。

## 不具合の報告

不具合を報告する際は、設定画面の「ログ」からログをコピーして添付してください。

## 開発者向け

### 必要な環境

- Windows または Linux x86_64
- Node.js 22.12 以上（CI は Node.js 24 LTS）と npm
- Rust stable と Cargo
- Windows: WebView2 Runtime と Tauri の Windows 開発用ツール
- Ubuntu 22.04 以降: Tauri の Linux 開発用パッケージ

```bash
sudo apt update
sudo apt install build-essential curl wget file libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev libwebkit2gtk-4.1-dev
```

### 開発とチェック

```powershell
npm ci
npm run tauri dev
```

```powershell
npm run check:version
npm run typecheck
npm run lint
npm test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Windows では WebView2 Runtime と Microsoft Edge WebDriver、Linux では `WebKitWebDriver` が必要です。`tauri-driver` はスクリプトが準備します。Linux のヘッドレス環境では `webkit2gtk-driver` と `xvfb` を入れ、`xvfb-run -a` の下で実行してください。

```powershell
npm run test:e2e
npm run test:e2e:screens
```

```bash
npm run test:e2e
xvfb-run -a npm run test:e2e:screens
```

配布物に同梱するフォントなどのライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) に記載しています。依存や同梱物を追加・更新するときはライセンス条件を確認し、必要な通知を更新してください。リリースは `v*` タグでビルドされ、GitHub Release の draft として作成されます。公開前に成果物とリリースノートを確認してください。

## ライセンス

YakuCraft のコードは [MIT License](LICENSE) で公開します。同梱フォントには別のライセンスが適用されます。詳細は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。
