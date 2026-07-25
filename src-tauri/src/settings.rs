//! アプリ設定の永続化。フロントの設定UIが持つ値を構造体で表し、アプリのデータディレクトリ直下の settings.json へ読み書きする。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// 設定を書き出すファイルの名前。アプリのデータディレクトリ直下に置く。
const SETTINGS_FILENAME: &str = "settings.json";




/// アプリの設定。フロントへそのまま渡すためフィールド名は camelCase で直列化する。serde(default) を付け、項目が増えても古いファイルが読めるようにする。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
	/// ダウンロードの保存先フォルダ。空文字列は未設定を表し、読み出し側で既定の保存先に置き換える。
	pub dest_dir: String,
	/// クリップボード監視が有効かどうか。
	pub watch: bool,
	/// 検出した投稿を確認せず自動ダウンロードするかどうか。
	pub auto_download: bool,
}










/// 設定ファイルのパス。アプリのデータディレクトリ直下に置く。
fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
	let dir = app
		.path()
		.app_data_dir()
		.map_err(|e| format!("データディレクトリの取得に失敗しました: {}", e))?;
	Ok(dir.join(SETTINGS_FILENAME))
}










/// 設定を読む。ファイルが無い・壊れている場合は既定値を返す。
pub fn load(app: &AppHandle) -> Settings {
	settings_path(app)
		.ok()
		.and_then(|path| fs::read_to_string(path).ok())
		.and_then(|text| serde_json::from_str(&text).ok())
		.unwrap_or_default()
}










/// 設定をファイルへ書き出す。親ディレクトリが無ければ作る。後から見て分かるよう整形して保存する。
pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
	let path = settings_path(app)?;

	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).map_err(|e| format!("ディレクトリの作成に失敗しました: {}", e))?;
	}

	let text = serde_json::to_string_pretty(settings).map_err(|e| format!("設定の直列化に失敗しました: {}", e))?;
	fs::write(&path, text).map_err(|e| format!("設定の書き込みに失敗しました: {}", e))
}
