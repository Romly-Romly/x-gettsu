<script lang="ts">
	import { invoke } from "@tauri-apps/api/core";
	import { listen } from "@tauri-apps/api/event";
	import { open } from "@tauri-apps/plugin-dialog";
	import { revealItemInDir } from "@tauri-apps/plugin-opener";
	import { onMount } from "svelte";

	type MediaItem = {
		kind: string; // "video" | "photo"
		best_url: string;
		thumbnail_url: string | null;
		width: number | null;
		height: number | null;
		bitrate: number | null;
		suggested_filename: string;
	};

	type MediaInfo = {
		screen_name: string;
		author_name: string;
		id: string;
		text: string;
		items: MediaItem[];
	};

	let destDir = $state("");
	let watch = $state(false);
	let autoDownload = $state(false);
	let urlInput = $state("");
	let info = $state<MediaInfo | null>(null);
	let status = $state("");
	let busy = $state(false);
	let progress = $state(0); // 0..1。total不明時は -1(不定)。
	// ダウンロード済みファイルの保存パスを、メディアの best_url をキーにして保持する対応表。各メディアを個別にゴミ箱へ削除できるようにするために使う。
	let savedPaths = $state<Record<string, string>>({});
	// 状態表示の「フォルダを開く」で開く対象。直近に保存したファイルのパスを指す。
	let lastSavedPath = $state("");
	let maximized = $state(false);
	// タイトルバーのアプリ名の右へ添えるバージョン。Rust の app_version が返す値で、取得できない間は空にしておき表示しない。
	let appVersion = $state("");
	// タイトルバーのドラッグ移動を始めるかを判断するための、押下時のカーソル位置。押していない間は null。
	let dragOrigin: { x: number; y: number } | null = null;

	// タイトルバーのキャプションボタンに使う Segoe Fluent Icons のグリフ。私用領域の符号位置のためソースへ直接埋め込まず符号値から生成する。
	const GLYPH_MINIMIZE = String.fromCharCode(0xe921);
	const GLYPH_MAXIMIZE = String.fromCharCode(0xe922);
	const GLYPH_RESTORE = String.fromCharCode(0xe923);
	const GLYPH_CLOSE = String.fromCharCode(0xe8bb);

	const URL_RE = /(?:twitter|x)\.com\/[A-Za-z0-9_]+\/status\/\d+/;

	onMount(async () => {
		destDir = localStorage.getItem("destDir") ?? (await invoke<string>("default_download_dir"));
		watch = localStorage.getItem("watch") === "1";
		autoDownload = localStorage.getItem("autoDownload") === "1";
		await invoke("set_watch", { enabled: watch });

		// システムのアクセントカラーを取得してテーマ変数へ反映する。取得できない場合は CSS 側の既定色を使う。
		try {
			const accent = await invoke<string | null>("accent_color");
			if (accent) document.documentElement.style.setProperty("--accent", accent);
		} catch {
			// 取得失敗時は既定色のまま
		}

		// タイトルバーへ表示するアプリのバージョンを取得する。値は tauri.conf.json 由来で、取得に失敗したら空のまま表示しない。
		try {
			appVersion = await invoke<string>("app_version");
		} catch {
			// 取得失敗時はバージョン非表示のまま
		}

		// テキスト入力欄以外での右クリックメニュー(webview 既定のもの)を抑止する。
		document.addEventListener("contextmenu", (e) => {
			const target = e.target as HTMLElement | null;
			if (!target?.closest("input, textarea")) e.preventDefault();
		});

		// カスタムタイトルバーの最大化ボタンの図形を、実際の最大化状態に追従させる。ボタンによる操作だけでなく Win+↑やスナップでの変化も含め、状態が変わると Rust 側が win-maximized を発火する。
		maximized = await invoke<boolean>("win_is_maximized");
		await listen<boolean>("win-maximized", (e) => {
			maximized = e.payload;
		});

		await listen<string>("clipboard-changed", async (e) => {
			if (!URL_RE.test(e.payload)) return;
			await handleDetectedUrl(e.payload);
		});

		await listen<{ downloaded: number; total: number | null }>("download-progress", (e) => {
			const { downloaded, total } = e.payload;
			progress = total && total > 0 ? downloaded / total : -1;
		});
	});

	function persist() {
		localStorage.setItem("destDir", destDir);
		localStorage.setItem("watch", watch ? "1" : "0");
		localStorage.setItem("autoDownload", autoDownload ? "1" : "0");
	}

	async function toggleWatch() {
		watch = !watch;
		persist();
		await invoke("set_watch", { enabled: watch });

		if (watch) {
			// 監視ONにした瞬間、いま入っているクリップボードも一度だけ確認する。
			try {
				const text = await invoke<string>("read_clipboard");
				if (URL_RE.test(text)) await handleDetectedUrl(text);
			} catch {
				// テキスト以外が入っている場合は無視
			}
		}
	}

	function toggleAuto() {
		autoDownload = !autoDownload;
		persist();
	}

	async function handleDetectedUrl(url: string) {
		try {
			status = "投稿を確認中…";
			const m = await invoke<MediaInfo>("inspect", { url });
			info = m;
			savedPaths = {};
			lastSavedPath = "";
			status = detectLabel(m);
			if (autoDownload) await startDownload(m.items);
		} catch {
			// 動画・画像なし等は監視中の誤爆を避けるため静かに無視する。
			info = null;
			status = "";
		}
	}

	async function manualInspect() {
		if (!urlInput.trim()) return;
		busy = true;
		info = null;
		savedPaths = {};
		lastSavedPath = "";
		try {
			status = "取得中…";
			info = await invoke<MediaInfo>("inspect", { url: urlInput.trim() });
			status = detectLabel(info);
		} catch (err) {
			status = `エラー: ${err}`;
		} finally {
			busy = false;
		}
	}

	async function chooseFolder() {
		const picked = await open({ directory: true, defaultPath: destDir || undefined });
		if (typeof picked === "string") {
			destDir = picked;
			persist();
		}
	}

	async function openDest() {
		if (!destDir) return;
		try {
			await invoke("open_dir", { path: destDir });
		} catch (err) {
			status = `フォルダを開けません: ${err}`;
		}
	}

	async function startDownload(items: MediaItem[]) {
		if (!items.length || busy) return;
		busy = true;
		progress = -1;
		const n = items.length;
		try {
			for (let i = 0; i < n; i++) {
				progress = -1;
				status = n > 1 ? `ダウンロード中… (${i + 1}/${n})` : "ダウンロード中…";
				const path = await invoke<string>("download", {
					url: items[i].best_url,
					destDir,
					filename: items[i].suggested_filename,
				});
				savedPaths[items[i].best_url] = path;
				lastSavedPath = path;
			}
			progress = 1;
			status = "ダウンロード完了";
		} catch (err) {
			status = `ダウンロード失敗: ${err}`;
		} finally {
			busy = false;
		}
	}

	async function reveal() {
		if (lastSavedPath) await revealItemInDir(lastSavedPath);
	}

	async function deleteItem(item: MediaItem) {
		const path = savedPaths[item.best_url];
		if (!path || busy) return;
		try {
			await invoke("move_to_trash", { paths: [path] });
			delete savedPaths[item.best_url];
			if (lastSavedPath === path) lastSavedPath = "";
			status = "ゴミ箱へ移動しました";
		} catch (err) {
			status = `削除に失敗: ${err}`;
		}
	}

	function resolutionLabel(item: MediaItem): string {
		return item.width && item.height ? `${item.width}×${item.height}` : "";
	}

	function kindLabel(item: MediaItem): string {
		return item.kind === "video" ? "動画" : "画像";
	}

	function detectLabel(m: MediaInfo): string {
		const videoCount = m.items.filter((it) => it.kind === "video").length;
		const photoCount = m.items.filter((it) => it.kind === "photo").length;
		const parts: string[] = [];
		if (videoCount > 0) parts.push(`動画${videoCount}件`);
		if (photoCount > 0) parts.push(`画像${photoCount}件`);
		return `メディアを検出: ${m.author_name}(${parts.join("・")})`;
	}

	// タイトルバーの押下位置を覚える。ここからしきい値を超えて動いたときだけウィンドウの移動を始める。
	function onDragMouseDown(e: MouseEvent) {
		dragOrigin = { x: e.screenX, y: e.screenY };
	}

	// 押下位置からしきい値(4px)を超えて動いたときだけ移動を始める。わずかな動きで移動を始めると、タイトルバーのダブルクリックによる最大化を奪ってしまう。
	function onDragMouseMove(e: MouseEvent) {
		if (dragOrigin && (e.buttons & 1) !== 0 && (Math.abs(e.screenX - dragOrigin.x) > 4 || Math.abs(e.screenY - dragOrigin.y) > 4)) {
			dragOrigin = null;
			invoke("win_start_drag");
		}
	}

	function onDragMouseUp() {
		dragOrigin = null;
	}

	// 最大化と元に戻すを切り替える。Rust 側が操作後の最大化状態を返すため、その値でボタンの図形を更新する。
	async function toggleMaximize() {
		maximized = await invoke<boolean>("win_toggle_maximize");
	}
</script>

<div class="titlebar">
	<div class="titlebar-drag" onmousedown={onDragMouseDown} onmousemove={onDragMouseMove} onmouseup={onDragMouseUp} ondblclick={toggleMaximize} role="presentation">
		<img class="titlebar-icon" src="/app-icon.png" alt="" aria-hidden="true" />
		<span class="titlebar-title">エックスげっつ</span>
		{#if appVersion}<span class="titlebar-version" aria-hidden="true">v{appVersion}</span>{/if}
	</div>
	<div class="titlebar-controls">
		<button class="tb-btn" onclick={() => invoke("win_minimize")} aria-label="最小化" title="最小化"><span class="tb-ico" aria-hidden="true">{GLYPH_MINIMIZE}</span></button>
		<button class="tb-btn" onclick={toggleMaximize} aria-label={maximized ? "元に戻す" : "最大化"} title={maximized ? "元に戻す" : "最大化"}><span class="tb-ico" aria-hidden="true">{maximized ? GLYPH_RESTORE : GLYPH_MAXIMIZE}</span></button>
		<button class="tb-btn tb-close" onclick={() => invoke("win_close")} aria-label="閉じる" title="閉じる"><span class="tb-ico" aria-hidden="true">{GLYPH_CLOSE}</span></button>
	</div>
</div>

<main class="container">
	<section class="card settings">
		<div class="path-row">
			<input
				class="path"
				type="text"
				placeholder="https://x.com/.../status/..."
				bind:value={urlInput}
				spellcheck="false"
				onkeydown={(e) => e.key === "Enter" && manualInspect()}
			/>
			<button class="primary" onclick={manualInspect} disabled={busy}>取得</button>
		</div>

		<label class="switch">
			<input type="checkbox" checked={watch} onchange={toggleWatch} />
			<span>クリップボードを監視して自動検出</span>
		</label>

		<label class="switch" class:disabled={!watch}>
			<input type="checkbox" checked={autoDownload} onchange={toggleAuto} disabled={!watch} />
			<span>検出したら確認せず自動ダウンロード</span>
		</label>
	</section>

	<section class="card">
		<div class="field">
			<span class="label">保存先</span>
			<div class="path-row">
				<input class="path" type="text" bind:value={destDir} onchange={persist} spellcheck="false" />
				<button class="ghost" onclick={openDest}>開く</button>
				<button class="ghost" onclick={chooseFolder}>変更…</button>
			</div>
		</div>
	</section>

	{#if info}
		{@const media = info}
		{#if media.items.length === 1}
			{@const item = media.items[0]}
			<section class="card media">
				{#if item.thumbnail_url}
					<img class="thumb" src={item.thumbnail_url} alt="サムネイル" />
				{/if}
				<div class="meta">
					<div class="author">{media.author_name} <span class="handle">@{media.screen_name}</span></div>
					<p class="text">{media.text}</p>
					<div class="badges">
						{#if resolutionLabel(item)}<span class="badge">{resolutionLabel(item)}</span>{/if}
						<span class="badge accent">最高画質</span>
						{#if savedPaths[item.best_url]}<span class="badge saved">保存済み</span>{/if}
					</div>
					{#if savedPaths[item.best_url]}
						<button class="ghost wide danger" onclick={() => deleteItem(item)} disabled={busy}>
							ゴミ箱へ削除
						</button>
					{:else}
						<button class="primary wide" onclick={() => startDownload(media.items)} disabled={busy}>
							ダウンロード
						</button>
					{/if}
				</div>
			</section>
		{:else}
			<section class="card media-multi">
				<div class="head">
					<div class="author">{media.author_name} <span class="handle">@{media.screen_name}</span></div>
					<p class="text">{media.text}</p>
				</div>
				<div class="items">
					{#each media.items as item}
						<div class="item-card">
							{#if item.thumbnail_url}
								<img class="thumb-sm" src={item.thumbnail_url} alt="サムネイル" />
							{/if}
							<div class="item-meta">
								<div class="badges">
									<span class="badge kind">{kindLabel(item)}</span>
									{#if resolutionLabel(item)}<span class="badge">{resolutionLabel(item)}</span>{/if}
									<span class="badge accent">最高画質</span>
									{#if savedPaths[item.best_url]}<span class="badge saved">保存済み</span>{/if}
								</div>
								{#if savedPaths[item.best_url]}
									<button class="ghost item-dl danger" onclick={() => deleteItem(item)} disabled={busy}>
										ゴミ箱へ削除
									</button>
								{:else}
									<button class="ghost item-dl" onclick={() => startDownload([item])} disabled={busy}>
										ダウンロード
									</button>
								{/if}
							</div>
						</div>
					{/each}
				</div>
				{#if media.items.some((it) => !savedPaths[it.best_url])}
					{@const remaining = media.items.filter((it) => !savedPaths[it.best_url])}
					<button class="primary wide" onclick={() => startDownload(remaining)} disabled={busy}>
						{remaining.length === media.items.length
							? `すべてダウンロード(${media.items.length}件)`
							: `残りをダウンロード(${remaining.length}件)`}
					</button>
				{/if}
			</section>
		{/if}
	{/if}

	{#if busy || progress === 1 || status}
		<div class="footer">
			{#if busy || progress === 1}
				<section class="card progress-card">
					<div class="bar">
						<div
							class="bar-fill"
							class:indeterminate={progress < 0}
							style={progress >= 0 ? `width:${Math.round(progress * 100)}%` : ""}
						></div>
					</div>
				</section>
			{/if}
			{#if status}
				<div class="status">
					<span>{status}</span>
					{#if lastSavedPath}
						<button class="ghost small" onclick={reveal}>フォルダを開く</button>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
</main>

<style>
	:root {
		/* onMount でシステムのアクセントカラーに差し替える。取得できなかった場合の既定値。 */
		--accent: #0078d4;
		font-family: "Segoe UI", system-ui, sans-serif;
		color: #1a1a1a;
		/* 背景はウィンドウのシステムバックドロップ(Mica/Acrylic)を透かすため透過にする。 */
		background-color: transparent;
	}

	:global(html),
	:global(body) {
		height: 100%;
	}

	:global(body) {
		margin: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	.titlebar {
		flex-shrink: 0;
		height: 32px;
		display: flex;
		align-items: stretch;
		user-select: none;
	}

	.titlebar-drag {
		flex: 1;
		display: flex;
		align-items: center;
		padding-left: 12px;
		min-width: 0;
	}

	.titlebar-icon {
		flex-shrink: 0;
		width: 16px;
		height: 16px;
		margin-right: 8px;
		pointer-events: none;
	}

	.titlebar-title {
		font-size: 0.72rem;
		font-weight: 500;
		color: #3a3a3a;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* アプリ名の右へ添えるバージョン。名前より一段淡い色と小さい字で控えめに見せ、名前が詰まっても縮めない。 */
	.titlebar-version {
		flex-shrink: 0;
		margin-left: 6px;
		font-size: 0.66rem;
		color: #8a8a8a;
		white-space: nowrap;
	}

	.titlebar-controls {
		display: flex;
		flex-shrink: 0;
	}

	.tb-btn {
		appearance: none;
		border: none;
		border-radius: 0;
		background: transparent;
		width: 46px;
		height: 32px;
		padding: 0;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: #1a1a1a;
		transition: background-color 0.12s;
	}

	.tb-ico {
		font-family: "Segoe Fluent Icons", "Segoe MDL2 Assets";
		font-size: 10px;
		line-height: 1;
	}

	.tb-btn:hover {
		background-color: rgba(0, 0, 0, 0.06);
	}

	.tb-close:hover {
		background-color: #c42b1c;
		color: #fff;
	}

	.tb-btn:focus-visible {
		outline-offset: -2px;
	}

	.container {
		box-sizing: border-box;
		width: 100%;
		max-width: 1600px;
		margin: 0 auto;
		padding: 20px 18px 16px;
		display: flex;
		flex-direction: column;
		gap: 14px;
		user-select: none;
		flex: 1;
		min-height: 0;
		/* レイアウト全体は固定し、ウィンドウに収まらない分は複数メディアのリスト(.items)の内部スクロールで受ける。 */
		overflow: hidden;
	}

	input,
	.text,
	.author,
	.handle,
	.status {
		user-select: text;
	}

	.card {
		background: #ffffff;
		border: 1px solid #e5e7eb;
		border-radius: 12px;
		padding: 14px;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
		flex-shrink: 0;
	}

	.settings {
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.label {
		font-size: 0.78rem;
		font-weight: 600;
		color: #6b7280;
	}

	.path-row {
		display: flex;
		gap: 8px;
	}

	.path {
		flex: 1;
		min-width: 0;
		border: 1px solid #d1d5db;
		border-radius: 8px;
		padding: 8px 10px;
		font-size: 0.85rem;
		background: #fafafa;
	}

	.path:focus {
		outline: none;
		border-color: var(--accent);
		background: #fff;
	}

	button {
		border: none;
		border-radius: 8px;
		padding: 8px 14px;
		font-size: 0.85rem;
		font-weight: 600;
		white-space: nowrap;
		transition: background 0.15s, opacity 0.15s;
	}

	button:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.primary {
		background: var(--accent);
		color: #fff;
	}

	.primary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent), black 12%);
	}

	.wide {
		width: 100%;
		margin-top: 10px;
		padding: 10px;
		flex-shrink: 0;
	}

	.ghost {
		background: #eef0f3;
		color: #374151;
	}

	.ghost:hover:not(:disabled) {
		background: #e2e5ea;
	}

	.small {
		padding: 4px 10px;
		font-size: 0.78rem;
	}

	.ghost.danger:hover:not(:disabled) {
		background: #fde2e0;
		color: #c42b1c;
	}

	.switch {
		display: flex;
		align-items: center;
		gap: 9px;
		font-size: 0.85rem;
	}

	.switch.disabled {
		color: #9ca3af;
		cursor: default;
	}

	.switch input {
		width: 16px;
		height: 16px;
		accent-color: var(--accent);
	}

	.media {
		display: flex;
		gap: 12px;
	}

	.media-multi {
		display: flex;
		flex-direction: column;
		gap: 12px;
		/* リストが長い時はカードを縮めて .items 側をスクロールさせるため、このカードだけは縮小を許可する。 */
		flex-shrink: 1;
		min-height: 0;
	}

	.head {
		min-width: 0;
		flex-shrink: 0;
	}

	.items {
		display: flex;
		flex-direction: column;
		gap: 10px;
		/* カードに収まらない分はリストの内側だけをスクロールさせる。 */
		min-height: 0;
		overflow-y: auto;
		scrollbar-width: thin;
		scrollbar-color: rgba(0, 0, 0, 0.28) transparent;
	}

	.item-card {
		display: flex;
		gap: 10px;
		align-items: center;
		padding: 8px;
		border: 1px solid #e5e7eb;
		border-radius: 10px;
		background: #fafafa;
	}

	.item-meta {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.item-dl {
		align-self: flex-start;
	}

	.thumb {
		width: 96px;
		height: 96px;
		object-fit: cover;
		border-radius: 8px;
		background: #e5e7eb;
		flex-shrink: 0;
	}

	.thumb-sm {
		width: 72px;
		height: 72px;
		object-fit: cover;
		border-radius: 8px;
		background: #e5e7eb;
		flex-shrink: 0;
	}

	.meta {
		flex: 1;
		min-width: 0;
	}

	.author {
		font-weight: 700;
		font-size: 0.9rem;
	}

	.handle {
		font-weight: 400;
		color: #6b7280;
		font-size: 0.8rem;
	}

	.text {
		margin: 4px 0 8px;
		font-size: 0.82rem;
		color: #374151;
		display: -webkit-box;
		-webkit-line-clamp: 3;
		line-clamp: 3;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.badges {
		display: flex;
		gap: 6px;
	}

	.badge {
		font-size: 0.72rem;
		font-weight: 600;
		padding: 2px 8px;
		border-radius: 999px;
		background: #eef0f3;
		color: #4b5563;
	}

	.badge.accent {
		background: var(--accent);
		color: #fff;
	}

	.badge.kind {
		background: #dbeafe;
		color: #1d4ed8;
	}

	.badge.saved {
		background: #dcfce7;
		color: #15803d;
	}

	.progress-card {
		padding: 12px 14px;
	}

	.bar {
		height: 8px;
		border-radius: 999px;
		background: #e5e7eb;
		overflow: hidden;
	}

	.bar-fill {
		height: 100%;
		background: var(--accent);
		border-radius: 999px;
		transition: width 0.2s;
	}

	.bar-fill.indeterminate {
		width: 40%;
		animation: slide 1.1s ease-in-out infinite;
	}

	@keyframes slide {
		0% { margin-left: -40%; }
		100% { margin-left: 100%; }
	}

	/* プログレスとステータスをウィンドウ最下部へ固定する。 */
	.footer {
		margin-top: auto;
		flex-shrink: 0;
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.status {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		font-size: 0.82rem;
		color: #4b5563;
		padding: 0 4px;
	}

	@media (prefers-color-scheme: dark) {
		:root {
			color: #e5e7eb;
			background-color: transparent;
		}

		.card {
			background: #26282e;
			border-color: #34373f;
			box-shadow: none;
		}

		.path {
			background: #1c1d22;
			border-color: #3a3d45;
			color: #e5e7eb;
		}

		.path:focus {
			background: #1c1d22;
		}

		.ghost {
			background: #34373f;
			color: #d1d5db;
		}

		.ghost:hover:not(:disabled) {
			background: #3e4149;
		}

		.label,
		.handle {
			color: #9ca3af;
		}

		.text {
			color: #c2c6cd;
		}

		.badge {
			background: #34373f;
			color: #cbd0d8;
		}

		.badge.kind {
			background: #1e3a5f;
			color: #93c5fd;
		}

		.badge.saved {
			background: #14432a;
			color: #86efac;
		}

		.item-card {
			background: #1c1d22;
			border-color: #34373f;
		}

		.thumb-sm {
			background: #34373f;
		}

		.bar {
			background: #34373f;
		}

		.status {
			color: #b6bbc4;
		}

		.ghost.danger:hover:not(:disabled) {
			background: #4a2320;
			color: #f2b8b2;
		}

		.titlebar-title {
			color: #c8c8c8;
		}

		.titlebar-version {
			color: #8a8a8a;
		}

		.tb-btn {
			color: #e8e8e8;
		}

		.tb-btn:hover {
			background-color: rgba(255, 255, 255, 0.08);
		}

		.items {
			scrollbar-color: rgba(255, 255, 255, 0.28) transparent;
		}
	}
</style>
