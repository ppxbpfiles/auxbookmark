# auxbookmark - PPx aux: Webブックマーク連携ツール

**auxbookmark** は、ファイラー「Paper Plane xUI (PPx)」の画面上で Web ブラウザのブックマークをファイルのように扱い、フォルダ分け・移動・複写・削除などの整理を行うためのツールです。  
PPx の `aux:` パス機能を利用し、Chromium系（Brave, Chrome, Edge など）や Firefox のブックマークを横断して一覧・操作できます（※おまけとして PPxの一行編集でブックマークと履歴検索するための auxbookmark_to_complist.js および Emacs 用の連携スクリプト `auxbookmark.el` も同梱しています）。

---

## 主な特徴

1. **マルチブラウザ＆複数プロファイル対応**:
   - ルート階層に各ブラウザ（`[Brave]` `[Chrome]` `[Edge]` `[Firefox]` など）が自動的にフォルダとして一覧表示されます。
   - Chromium系（JSON形式）と Firefox系（SQLite形式）を透過的に統合し、PPc の 2 画面操作（`M`/`C`）で **Brave ⇔ Firefox 間のブックマーク相互移動・複写** も可能です。
   - `auxbookmark.ini` を使えば、特定プロファイルや互換ブラウザのパスを自由に追加・固定できます。
2. **2 画面ファイラー操作による整理**:
   - `M` キー（移動）や `C` キー（複写）により、同一ブラウザ内およびブラウザ間のブックマーク移動・複写が可能です。
   - `K` キーで階層内に新規フォルダを作成できます。
3. **Enter キーで即時ブラウザ起動**:
   - ブックマーク（`.url`）上で `Enter` を押すと、一時ショートカット（`[InternetShortcut]`）を生成し、Windows の既定ブラウザで即座に開きます。
4. **ゴミ箱方式による誤削除防止**:
   - 通常フォルダ内で `Delete` を押した場合、即座に消滅させずブックマーク内の「ゴミ箱 (Trash)」フォルダへ安全に退避します。
   - 「ゴミ箱」フォルダ内で `Delete` を押した場合のみ、完全削除されます。
5. **ブラウザごとの独立・自動バックアップ**:
   - 変更操作（移動・作成・削除等）の初回実行時に、`backups/` フォルダへ `{ブラウザ名}_Bookmarks_YYYYMMDD_HHMMSS.bak` を自動保存します。
   - 連続して整理操作を行っても無駄なバックアップは作られず（デフォルト30分間隔）、整理セッション開始時の状態が保護されます。
6. **Netscape Bookmark HTML 形式へのエクスポート**:
   - 他のブラウザや外部ツールへインポート可能な標準 HTML 形式への書き出しに対応しています（ゴミ箱フォルダの自動除外にも対応）。
7. **並行死活チェック（リンクチェック機能）**:
   - 登録されている Web ブックマークの URL をマルチスレッドで並行検証し、リンク切れ（404、ドメイン失効、サーバー停止、タイムアウト）を高速検出します。
   - SSL証明書エラーによる誤判定防止処理を備え、検出されたリンク切れアイテムを自動的に「ゴミ箱」または「リンク切れ」フォルダへ退避できます。
8. **閲覧履歴の統合とブックマーク化**:
   - 各ブラウザのルート直下に仮想フォルダ `[履歴]` が配置され、直近 500 件の訪問履歴を一覧表示・`Enter` 起動できます。
   - ブラウザ起動中であっても一時コピーによるロック回避により安全に取得できます。
   - PPc の 2 画面操作で `[履歴]` 内のアイテムを反対画面のブックマークフォルダへ `C`（複写）キーで直接登録できます。
   - 誤操作を防ぐため、履歴フォルダ内のアイテムに対する移動・削除・フォルダ作成は自動的に保護（読み取り専用）されます。

---

## ディレクトリ構成

```text
PPxインストール先/
├── auxcmd/
│   ├── auxbookmark.exe        # 本ツールの実行ファイル
│   ├── auxbookmark.ini        # 設定ファイル（任意）
│   └── backups/               # 自動バックアップ格納フォルダ
```

---

## 導入手順

### 1. ビルド
```powershell
cd auxbookmark
powershell -ExecutionPolicy Bypass -File .\build.ps1
```
カレントフォルダに `auxbookmark.exe`が生成されます。
これを PPx の `auxcmd` フォルダ（例: `C:\PPx\auxcmd\`）へ配置します。

### 2. 設定ファイルの配置（任意）
`auxbookmark.ini.sample` を `auxbookmark.ini` にリネームして `auxbookmark.exe` と同じ場所に配置します。
PC に存在するブラウザ（Brave / Chrome / Edge）は自動検出されるため、設定ファイルなしでも動作します。

```ini
[general]
; 整理セッション開始時の自動バックアップ間隔（分）
backup_interval_minutes = 30

[profiles]
; 特定のプロファイルや名前を追加・固定したい場合
; Brave = C:\Users\user\AppData\Local\BraveSoftware\Brave-Browser\User Data\Default\Bookmarks
; Chrome = C:\Users\user\AppData\Local\Google\Chrome\User Data\Default\Bookmarks
; Work   = C:\Users\user\AppData\Local\Google\Chrome\User Data\Profile 1\Bookmarks
```

### 3. PPx への設定取り込み
`auxbookmark.cfg` を PPx に取り込みます。

- **コマンドの場合**:
  ```cmd
  PPCUSTW.EXE CA auxbookmark.cfg
  ```

---

## 仮想エントリとオンデマンド実体化の仕様

PPc 上のリストに表示される `.url` ファイルは、ローカルディスク上に保存された実体ファイルではなく、ブラウザのデータベース（SQLite / JSON）から動的に生成された仮想エントリ（ListFile）です。

- **一覧取得時**: ブラウザの履歴やブックマーク情報を読み取ってリストを構築します。ローカルディスク上に `.url` ファイルは作成されません。
- **実行時**: エントリを実行した（Enter）時点で、該当 URL をブラウザで開きます。
- **コピー・マーク抽出時**: PPx のコピー操作等を行うと、指定された出力先フォルダへ実際のインターネットショートカット（`.url` ファイル）が生成・出力されます。

---

## 使い方

PPc のアドレスバーで以下のパスを入力して移動します：

```text
aux://S_auxbookmark/
```

### 階層ツリーの構成
```text
aux://S_auxbookmark/
├── [Brave]
│   ├── ブックマーク バー/
│   ├── その他のブックマーク/
│   ├── ゴミ箱/
│   └── 履歴/              # 直近500件の閲覧履歴（読み取り専用）
├── [Firefox]
│   ├── ブックマーク バー/
│   ├── その他のブックマーク/
│   ├── ブックマーク メニュー/
│   ├── ゴミ箱/
│   └── 履歴/              # 直近500件の閲覧履歴（読み取り専用）
├── [Chrome]
│   └── ...
└── [Edge]
    └── ...
```

### 基本操作（PPc 上）
- **フォルダ移動 (`M`)**: 反対画面のフォルダ（別ブラウザも可）へ移動します。
- **フォルダ複写 (`C`)**: 反対画面のフォルダ（別ブラウザも可）へ複製します。
- **閲覧履歴のブックマーク化 (`C`)**: `[履歴]` フォルダ内のアイテムを選択し、反対画面のブックマークフォルダへ `C`（複写）を押すだけで簡単にブックマーク登録できます。
- **フォルダ作成 (`K`)**: 新規フォルダを作成します。
  - **※ 注意**: ブラウザの構造上の仕様により、ブラウザ名（`[Brave]` や `[Pale Moon]` など）の直下には直接フォルダを作成できません。新規フォルダを作成する際は、必ず「ブックマーク バー」や「その他のブックマーク」等の**中に入った状態**で `K` を押してください。
- **名前変更 (`R`)**: ブックマークやフォルダの名前を変更します。
- **削除 (`D`)**:
  - 通常フォルダ内: 「ゴミ箱」へ移動（退避）
  - ゴミ箱フォルダ内: 完全消去
  - 履歴フォルダ内: 読み取り専用のため安全に保護（削除不可）

---

## コマンドラインツールとしての単体利用

`auxbookmark.exe` はコマンドラインから直接実行してエクスポートやリンクチェックを行うことも可能です。

```text
Usage: auxbookmark <command> [arguments...]
```

### 1. HTMLエクスポート (`export`)
Netscape Bookmark HTML 形式でブックマークをファイルに書き出します。

```powershell
# ゴミ箱を除外したクリーンエクスポート（既定）
auxbookmark export Brave

# 出力先ファイルを指定
auxbookmark export Firefox C:\backup\firefox_clean.html

# ゴミ箱も含めてすべてエクスポート
auxbookmark export Brave --all
```

### 2. 並行死活チェック (`check`)
マルチスレッドで URL の生存確認を行い、リンク切れを検出・レポートします。

```powershell
# レポート表示のみ（データは変更しません）
auxbookmark check Brave
auxbookmark check "Firefox/ブックマーク バー/PC関係"

# リンク切れを自動的に「ゴミ箱」フォルダへ退避
auxbookmark check Firefox --trash

# リンク切れを「リンク切れ」フォルダへ隔離退避
auxbookmark check Brave --isolate

# タイムアウト秒数や並行スレッド数を指定
auxbookmark check Brave --timeout 10 --threads 16
```

### 3. TSVダンプ (`dump`)
全ブラウザ（または指定ブラウザ）のブックマークおよび直近の閲覧履歴をタブ区切り形式（タイトル、URL、ブラウザ名、フォルダパス）で一括出力します（外部ツール連携用）。

```powershell
# 全ブラウザのブックマークおよび閲覧履歴を出力
auxbookmark dump

# 特定ブラウザのみ出力
auxbookmark dump Firefox
```

---

## おまけ

### PPx 一行編集でのブックマーク・履歴検索 (`auxbookmark_to_complist.js`)

PPx の一行編集（`%*input`）補完機能を使い、全ブラウザのブックマークおよび閲覧履歴をインクリメンタル検索して既定ブラウザで開くための JScript と PPx 設定ファイルを `omake/ppx/` フォルダに同梱しています。

- `auxbookmark_to_complist.js` : `auxbookmark dump` の TSV 出力を PPx 補完リスト形式に変換するスクリプト
- `auxbookmark_ppx.cfg` : PPx キー設定サンプル（既定: `Alt+B` で補完検索を起動）

**導入手順**:
1. `auxbookmark_to_complist.js` を PPx の `script` フォルダ（`%0script\`）に配置します。
2. `auxbookmark_ppx.cfg` を PPcust の「設定の読み込み」で取り込みます。

### Emacs (Consult) 連携 (`auxbookmark.el`)

Emacs 上で Consult を使い、全ブラウザのブックマークおよび閲覧履歴を一括でインクリメンタル横断検索して既定ブラウザで開くための連携スクリプト `auxbookmark.el` を `omake/emacs/` フォルダに同梱しています。

詳細な導入方法や設定例については [`README_emacs.md`](omake/emacs/README_emacs.md) をご覧ください。

```elisp
;; 簡易設定例 (init.el)
(add-to-list 'load-path "C:/path/to/auxbookmark/omake/emacs")
(require 'auxbookmark)
(global-set-key (kbd "C-c b") 'consult-auxbookmark)
```

---

## 免責事項（Disclaimer）

本ツール（`auxbookmark.exe`）は、Paper Plane xUI（PPx）の `aux:` パス機能を利用して有志が個人制作した非公式の連携ツールです。プログラムやドキュメントの大部分は AI との対話を通じて作成されています。  
PPx の作者である TORO 氏の著作物ではありません。本ツールに関する質問・要望・不具合報告などを TORO 氏へ問い合わせることはご遠慮ください。

