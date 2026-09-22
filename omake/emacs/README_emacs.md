# auxbookmark.el - Emacs Consult 連携（おまけ）

`auxbookmark.exe` をバックエンドとして利用し、Emacs 上で全ブラウザ（Brave, Firefox, Chrome, Edge など）の Web ブックマークおよび直近の閲覧履歴を Consult によりインクリメンタル横断検索し、既定ブラウザで開くための連携スクリプトです。

> **位置づけ**: 本スクリプトは有志による連携機能のおまけ（参考実装）です。

---

## 主な特徴

1. **全ブラウザ・閲覧履歴の横断検索**:
   - `auxbookmark.ini` に設定されている（または自動検出された）ブラウザのブックマークと閲覧履歴をまとめて検索できます。
2. **Consult インクリメンタル絞り込み**:
   - `consult--read` を活用し、タイトル、ブラウザ名、フォルダ階層のいずれからでもリアルタイムに絞り込めます。
   - 例: `Amazon`（タイトルで絞り込み）、`Firefox`（ブラウザで絞り込み）、`Brave 投資`（アンド絞り込み）など。
3. **メモリキャッシュによる高速起動**:
   - 初回取得後はメモリ上にキャッシュされ、2回目以降はキャッシュから読み込みます。
   - `C-u M-x consult-auxbookmark` を実行すると、最新のブックマークを強制再取得します。
4. **既定ブラウザでの起動**:
   - 候補を選択して `Enter` を押すと、`browse-url` により Windows の既定ブラウザでその Web ページが開きます。

---

## 前提条件

- `auxbookmark.exe` （および `auxbookmark.ini`）が環境変数 `PATH` の通った場所、または Emacs の実行ファイルディレクトリ等に存在すること（または `auxbookmark-program` 変数でパスを指定）。
- Emacs に `consult` パッケージがインストールされていること（※Consult がない場合は標準の `completing-read` で動作します）。

---

## 導入方法 (`init.el`)

お使いの `init.el` に以下を記述します。

### 基本的な設定例

```elisp
;; auxbookmark.el の配置先フォルダを load-path に追加
(add-to-list 'load-path "~/.emacs.d/lisp")

(require 'auxbookmark nil t)

(when (featurep 'auxbookmark)
  ;; キーバインド例: C-c b でブックマーク横断検索
  (global-set-key (kbd "C-c b") 'consult-auxbookmark))
```

### use-package を使う場合

```elisp
(use-package auxbookmark
  :load-path "~/.emacs.d/lisp"
  :bind ("C-c b" . consult-auxbookmark))
```

---

## コマンド一覧

| コマンド | 説明 |
| :--- | :--- |
| `M-x consult-auxbookmark`<br>（または `M-x auxbookmark-consult`） | 全ブラウザのブックマークを Consult 検索し、選択した URL を開く。<br>※`C-u` 付きで実行するとキャッシュを破棄して最新化。 |
| `M-x auxbookmark-clear-cache` | メモリ内のブックマークキャッシュを消去する。 |
