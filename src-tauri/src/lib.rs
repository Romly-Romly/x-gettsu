use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

mod settings;

/// video.twimg.com は素のクライアントだと弾くことがあるため、ブラウザを装うUAを使う。
const USER_AGENT: &str =
	"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// クリップボードを監視するかどうかの共有フラグ。バックグラウンドスレッドとコマンドが参照する。
struct WatchFlag(Arc<AtomicBool>);




// fxtwitter API (api.fxtwitter.com) のレスポンスのうち、必要なフィールドだけを取り出す型。

#[derive(Deserialize)]
struct FxResponse {
	#[allow(dead_code)]
	code: i32,
	tweet: Option<FxTweet>,
}




#[derive(Deserialize)]
struct FxTweet {
	text: String,
	author: FxAuthor,
	media: Option<FxMedia>,
}




#[derive(Deserialize)]
struct FxAuthor {
	name: String,
}




#[derive(Deserialize)]
struct FxMedia {
	#[serde(default)]
	videos: Vec<FxVideo>,
	#[serde(default)]
	photos: Vec<FxPhoto>,
}




#[derive(Deserialize)]
struct FxVideo {
	/// fxtwitter が選んだ最高画質mp4のURL。variants から選べない場合のフォールバックに使う。
	url: String,
	#[serde(default)]
	thumbnail_url: Option<String>,
	#[serde(default)]
	width: Option<u32>,
	#[serde(default)]
	height: Option<u32>,
	#[serde(default)]
	variants: Vec<FxVariant>,
}




#[derive(Deserialize)]
struct FxVariant {
	#[serde(default)]
	content_type: Option<String>,
	#[serde(default)]
	bitrate: Option<u64>,
	url: String,
}




#[derive(Deserialize)]
struct FxPhoto {
	url: String,
	#[serde(default)]
	width: Option<u32>,
	#[serde(default)]
	height: Option<u32>,
}




/// フロントへ返す、投稿内の1つの動画または画像。
#[derive(Serialize)]
struct MediaItem {
	/// メディアの種別。動画なら "video"、画像なら "photo"。
	kind: String,
	/// ダウンロード対象に選んだ最高画質のURL。
	best_url: String,
	thumbnail_url: Option<String>,
	width: Option<u32>,
	height: Option<u32>,
	bitrate: Option<u64>,
	suggested_filename: String,
}




/// フロントへ返す、解析済みのメディア情報(動画・画像)。
#[derive(Serialize)]
struct MediaInfo {
	screen_name: String,
	author_name: String,
	id: String,
	text: String,
	items: Vec<MediaItem>,
}




/// ダウンロードの進捗。total はサーバが Content-Length を返さない場合 None。
#[derive(Serialize, Clone)]
struct DownloadProgress {
	downloaded: u64,
	total: Option<u64>,
}




/// 投稿URLから screen_name と status ID を抜き出す。x.com / twitter.com の両方に対応。
fn parse_status_url(url: &str) -> Option<(String, String)> {
	let re = regex::Regex::new(r"(?:twitter|x)\.com/([A-Za-z0-9_]+)/status/(\d+)").ok()?;
	let caps = re.captures(url)?;
	Some((caps.get(1)?.as_str().to_string(), caps.get(2)?.as_str().to_string()))
}




/// Windows で使えないファイル名文字を `_` に置き換える。
fn sanitize_filename(name: &str) -> String {
	name.chars()
		.map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
		.collect()
}




/// 画像URLから拡張子を推定する。クエリを除いた末尾パスセグメントの最後の `.` 以降を小文字で返し、取れなければ `jpg` とする。
fn image_extension(url: &str) -> String {
	let path = url.split('?').next().unwrap_or(url);
	let segment = path.rsplit('/').next().unwrap_or(path);

	match segment.rsplit_once('.') {
		Some((_, ext)) if !ext.is_empty() => ext.to_ascii_lowercase(),
		_ => "jpg".to_string(),
	}
}




/// 保存ファイル名を組み立て、使えない文字を除去して返す。items が1件のときは連番を付けず、複数のときは items 全体での通し番号 index を付ける。
fn suggested_filename(screen_name: &str, id: &str, index: usize, total: usize, ext: &str) -> String {
	let name = if total == 1 {
		format!("{}_{}.{}", screen_name, id, ext)
	} else {
		format!("{}_{}_{}.{}", screen_name, id, index, ext)
	};

	sanitize_filename(&name)
}




/// 保存先に同名ファイルがあれば ` (2)`, ` (3)` … を付けて衝突を避ける。
fn unique_path(dir: &Path, filename: &str) -> PathBuf {
	let candidate = dir.join(filename);

	if !candidate.exists() {
		return candidate;
	}

	let path = Path::new(filename);
	let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
	let ext = path.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();

	let mut n = 2;

	loop {
		let candidate = dir.join(format!("{} ({}){}", stem, n, ext));

		if !candidate.exists() {
			return candidate;
		}

		n += 1;
	}
}




/// 既定の保存先フォルダを返す。OSのダウンロードフォルダ直下の X-Gettsu を指し、ダウンロードフォルダを取得できない環境ではホームフォルダを基準にする。
#[cfg(not(debug_assertions))]
fn default_dir(app: &AppHandle) -> PathBuf {
	let base = app
		.path()
		.download_dir()
		.or_else(|_| app.path().home_dir())
		.unwrap_or_else(|_| PathBuf::from("."));
	base.join("X-Gettsu")
}




/// デバッグビルドでの既定保存先。ビルド時のソース位置を基準に、プロジェクト直下の downloads を指す。ソースのパスがバイナリへ埋め込まれるため、この実装は配布するリリースビルドでは使わない。
#[cfg(debug_assertions)]
fn default_dir(_app: &AppHandle) -> PathBuf {
	let manifest = env!("CARGO_MANIFEST_DIR");
	let root = Path::new(manifest)
		.parent()
		.map(|p| p.to_path_buf())
		.unwrap_or_else(|| PathBuf::from("."));
	root.join("downloads")
}




/// 投稿URLを fxtwitter 経由で解析し、最高画質の動画・画像情報を返す。
#[tauri::command]
async fn inspect(url: String) -> Result<MediaInfo, String> {
	let (screen_name, id) =
		parse_status_url(&url).ok_or_else(|| "X(Twitter)の投稿URLとして認識できませんでした".to_string())?;

	let api = format!("https://api.fxtwitter.com/{}/status/{}", screen_name, id);

	let client = reqwest::Client::builder()
		.user_agent(USER_AGENT)
		.build()
		.map_err(|e| e.to_string())?;

	let resp = client.get(&api).send().await.map_err(|e| e.to_string())?;

	if !resp.status().is_success() {
		return Err(format!("API応答エラー: HTTP {}", resp.status().as_u16()));
	}

	let data: FxResponse = resp.json().await.map_err(|e| format!("JSON解析に失敗: {}", e))?;
	let tweet = data.tweet.ok_or_else(|| "投稿を取得できませんでした".to_string())?;

	let (videos, photos) = match tweet.media {
		Some(media) => (media.videos, media.photos),
		None => (Vec::new(), Vec::new()),
	};

	let total = videos.len() + photos.len();
	let mut items: Vec<MediaItem> = Vec::with_capacity(total);
	let mut index = 1usize;

	// 動画は各要素の variants から content_type が mp4 かつ bitrate 最大のものを選ぶ。該当が無ければ fxtwitter 既定のURLを使い bitrate は不明とする。
	for video in videos {
		let (best_url, bitrate) = video
			.variants
			.iter()
			.filter(|v| v.content_type.as_deref().map_or(false, |c| c.contains("mp4")))
			.filter(|v| v.bitrate.is_some())
			.max_by_key(|v| v.bitrate.unwrap_or(0))
			.map(|v| (v.url.clone(), v.bitrate))
			.unwrap_or((video.url.clone(), None));

		items.push(MediaItem {
			kind: "video".to_string(),
			best_url,
			thumbnail_url: video.thumbnail_url,
			width: video.width,
			height: video.height,
			bitrate,
			suggested_filename: suggested_filename(&screen_name, &id, index, total, "mp4"),
		});

		index += 1;
	}

	// fxtwitter が返す画像URLは拡張子付きパスに name=orig クエリが付いた形式(例: .../{ID}.jpg?name=orig)。既存のクエリを一旦取り除き、拡張子を残したまま name=orig を付け直すことで、常に最高画質(orig)の単一パラメータにする。pbs.twimg.com は拡張子がパスに残っている場合のみ name=orig で 200 を返すため、拡張子はパスから外さない。サムネイルにも fxtwitter の元URLをそのまま使う。
	for photo in photos {
		let base = photo.url.split('?').next().unwrap_or(photo.url.as_str());
		let best_url = format!("{}?name=orig", base);
		let ext = image_extension(&photo.url);

		items.push(MediaItem {
			kind: "photo".to_string(),
			best_url,
			thumbnail_url: Some(photo.url),
			width: photo.width,
			height: photo.height,
			bitrate: None,
			suggested_filename: suggested_filename(&screen_name, &id, index, total, &ext),
		});

		index += 1;
	}

	if items.is_empty() {
		return Err("この投稿に動画・画像はありません".to_string());
	}

	Ok(MediaInfo {
		screen_name,
		author_name: tweet.author.name,
		id,
		text: tweet.text,
		items,
	})
}




/// 指定URLのメディアを dest_dir へストリーミング保存し、進捗を download-progress イベントで通知する。
#[tauri::command]
async fn download(app: AppHandle, url: String, dest_dir: String, filename: String) -> Result<String, String> {
	let dir = PathBuf::from(&dest_dir);
	std::fs::create_dir_all(&dir).map_err(|e| format!("保存先フォルダを作成できません: {}", e))?;

	let path = unique_path(&dir, &filename);

	let client = reqwest::Client::builder()
		.user_agent(USER_AGENT)
		.build()
		.map_err(|e| e.to_string())?;

	let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;

	if !resp.status().is_success() {
		return Err(format!("ダウンロード失敗: HTTP {}", resp.status().as_u16()));
	}

	let total = resp.content_length();
	let mut file = std::fs::File::create(&path).map_err(|e| format!("ファイルを作成できません: {}", e))?;
	let mut downloaded: u64 = 0;
	let mut stream = resp.bytes_stream();

	while let Some(chunk) = stream.next().await {
		let chunk = chunk.map_err(|e| format!("受信中にエラー: {}", e))?;
		file.write_all(&chunk).map_err(|e| format!("書き込みエラー: {}", e))?;
		downloaded += chunk.len() as u64;
		let _ = app.emit("download-progress", DownloadProgress { downloaded, total });
	}

	file.flush().map_err(|e| format!("書き込みの確定に失敗: {}", e))?;

	Ok(path.to_string_lossy().to_string())
}




/// 現在のクリップボードのテキストを返す。テキスト以外が入っている場合はエラー。
#[tauri::command]
fn read_clipboard() -> Result<String, String> {
	let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
	cb.get_text().map_err(|e| e.to_string())
}




/// クリップボード監視の有効/無効を切り替える。
#[tauri::command]
fn set_watch(state: State<WatchFlag>, enabled: bool) {
	state.0.store(enabled, Ordering::Relaxed);
}




/// 保存済みのアプリ設定を返す。保存先フォルダが未設定の場合は既定の保存先フォルダで埋めて返す。
#[tauri::command]
fn get_settings(app: AppHandle) -> settings::Settings {
	let mut loaded = settings::load(&app);

	if loaded.dest_dir.is_empty() {
		loaded.dest_dir = default_dir(&app).to_string_lossy().to_string();
	}

	loaded
}




/// アプリ設定を settings.json へ書き出す。
#[tauri::command]
fn save_settings(app: AppHandle, settings: settings::Settings) -> Result<(), String> {
	settings::save(&app, &settings)
}




/// 保存先フォルダをファイルマネージャーで開く。まだ存在しない場合は作成してから開くため、初回ダウンロード前でも利用できる。
#[tauri::command]
fn open_dir(app: AppHandle, path: String) -> Result<(), String> {
	use tauri_plugin_opener::OpenerExt;

	std::fs::create_dir_all(&path).map_err(|e| format!("フォルダを作成できません: {}", e))?;
	app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}




/// 指定したファイル群をOSのゴミ箱へ移動する。ダウンロード直後に不要と判断した分を、完全削除ではなく復元可能な形で取り消すために使う。
#[tauri::command]
fn move_to_trash(paths: Vec<String>) -> Result<(), String> {
	if paths.is_empty() {
		return Ok(());
	}

	trash::delete_all(&paths).map_err(|e| format!("ゴミ箱へ移動できません: {}", e))
}




/// タイトルバーへ表示するアプリのバージョンを返す。文字列は tauri.conf.json 由来の PackageInfo から引くため、バージョンを変えてもここが追従し、表示が二重管理にならない。
#[tauri::command]
fn app_version(app: AppHandle) -> String {
	app.package_info().version.to_string()
}




#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
	let flag = Arc::new(AtomicBool::new(false));
	let flag_for_thread = flag.clone();

	tauri::Builder::default()
		.plugin(tauri_plugin_opener::init())
		.plugin(tauri_plugin_dialog::init())
		// ウィンドウの位置・サイズ・最大化状態を次回起動へ引き継ぐ。移動・リサイズの捕捉と終了時の書き出しをプラグインが担い、前回の配置の復元は setup で行う。
		.plugin(romly_tauri_common::window_state::plugin())
		.manage(WatchFlag(flag))
		.setup(move |app| {
			let handle = app.handle().clone();

			// 前回のウィンドウ位置・サイズを、現在のモニター構成へ合わせて補正してから復元する。表示の前に整えることで、初期表示で位置やサイズが飛ぶのを避ける。
			romly_tauri_common::window_state::restore(app.handle());
			// メインウィンドウは transparent かつ visible:false で生成する。透過ウィンドウを背景の適用前に見せると一瞬デスクトップが透けるため、バックドロップ(Mica/Acrylic)を当ててから表示する。
			romly_tauri_common::apply_backdrop(app.handle());
			if let Some(window) = app.get_webview_window("main") {
				let _ = window.show();
			}

			// OS のアクセント色の変更をフロントへ伝え、起動後に色を変えてもテーマがその場で追従するようにする。
			romly_tauri_common::watch_accent_color(app.handle());

			// クリップボードを定期的に読み、内容が変わったら(監視ON時のみ) clipboard-changed を発火する。
			std::thread::spawn(move || {
				let mut cb = match arboard::Clipboard::new() {
					Ok(c) => c,
					Err(_) => return,
				};

				// 起動時点でクリップボードにある内容は、監視を始める前からの基準として扱い、変化イベントとして発火させない。これにより、監視を有効にしたまま終了し、同じURLがクリップボードに残ったまま再起動しても、そのURLを再びダウンロードしてしまうのを防ぐ。
				let mut last = cb.get_text().unwrap_or_default();

				loop {
					std::thread::sleep(Duration::from_millis(800));

					if let Ok(text) = cb.get_text() {
						if text != last && !text.is_empty() {
							last = text.clone();

							if flag_for_thread.load(Ordering::Relaxed) {
								let _ = handle.emit("clipboard-changed", text);
							}
						}
					}
				}
			});

			Ok(())
		})
		.invoke_handler(tauri::generate_handler![
			inspect,
			download,
			read_clipboard,
			set_watch,
			get_settings,
			save_settings,
			open_dir,
			move_to_trash,
			app_version,
			romly_tauri_common::accent_color,
			romly_tauri_common::win_minimize,
			romly_tauri_common::win_toggle_maximize,
			romly_tauri_common::win_is_maximized,
			romly_tauri_common::win_start_drag,
			romly_tauri_common::win_close
		])
		.run(tauri::generate_context!())
		.expect("error while running tauri application");
}
