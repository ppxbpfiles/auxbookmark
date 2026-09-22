//!*script
// ============================================================================
// スクリプト名  : auxbookmark_to_complist.js
//
// 目的
//
// 本スクリプトは、auxbookmark.exe dump の出力(TSV)を PPx の
// 一行編集に渡すため、補完リストをスクリプトで加工し、タイトル・
// ブラウザ情報を行頭コメントに分離する。
//
// 【全体の流れ】
//   1. 第1引数(PPx.Argument(0))で指定された入力ファイル(TSV、1行1件)を読む
//      (列構成: タイトル\tURL\tブラウザ名\tフォルダパス)
//   2. 各行を「;<タイトル [ブラウザ/フォルダ]>; URL」の形式(行頭コメント)に
//      変換し、一時ファイル(%'temp'%\bkcomplist.txt)に書き直す
//      → こうすることで、PPx上で補完候補を選ぶ際にタイトル等がコメントとして
//        見えつつ、実際に入力されるのは「URL」になる
//
// 【前提条件】
//   ・第1引数には auxbookmark.exe dump の出力を保存したTSVファイルのパスを渡す
//   ・入出力ともにUTF-8として扱う(ADODB.Streamを使用)
// ============================================================================

var fso = PPx.CreateObject("Scripting.FileSystemObject");

// 引数チェック
var tsvfile = PPx.Argument(0);
if (tsvfile === "") {
    PPx.Echo("引数エラー: 入力ファイルのパスを第1引数で指定してください");
    PPx.Quit(-1); throw "abort";
}

// 入力ファイル存在チェック
if (!fso.FileExists(tsvfile)) {
    PPx.Echo("入力ファイルが見つかりません: " + tsvfile);
    PPx.Quit(-1); throw "abort";
}

var listfile = PPx.Extract("%'temp'%") + "\\bkcomplist.txt";

var inStream = PPx.CreateObject("ADODB.Stream");
inStream.Type    = 2;
inStream.Charset = "utf-8";
inStream.Open();
inStream.LoadFromFile(tsvfile);
var text = inStream.ReadText();
inStream.Close();

// 各行を行頭コメント形式に変換する
var lines = text.split(/\r\n|\n/);
var out   = [];
for (var i = 0; i < lines.length; i++) {
    var cols = lines[i].split("\t");
    if (cols.length < 2 || cols[0] === "") continue;
    var title   = cols[0];
    var url     = cols[1];
    var browser = cols.length >= 3 ? cols[2] : "";
    var path    = cols.length >= 4 ? cols[3] : "";
    out.push(";<" + title + " [" + browser + "/" + path + "]>; " + url);
}

// UTF-8(BOM付き)で書き出す
var outStream = PPx.CreateObject("ADODB.Stream");
outStream.Type    = 2;
outStream.Charset = "utf-8";
outStream.Open();
outStream.WriteText(out.join("\r\n") + "\r\n");
outStream.SaveToFile(listfile, 2); // 2 = adSaveCreateOverWrite
outStream.Close();
