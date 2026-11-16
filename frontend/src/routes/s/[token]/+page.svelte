<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import {
		api,
		publicContentUrl,
		publicDownloadUrl,
		publicThumbUrl,
		publicZipUrl,
		type PublicFileNode,
		type PublicListing,
		type PublicShare
	} from '$lib/api';
	import { RequestScope, isAbortError } from '$lib/generations';
	import {
		PREVIEW_MAX_BYTES,
		PREVIEW_PREFIX_BYTES,
		readTextPrefix,
		truncateCodePoints,
		type PreviewState
	} from '$lib/preview';

	let token = '';
	let loading = true;
	let errorMsg = '';
	let share: PublicShare | null = null;
	let listing: PublicListing | null = null;
	let selected: PublicFileNode | null = null;
	let previewText = '';
	let previewState: PreviewState = 'idle';
	let renderLimit = 200;
	let truncated = false;
	let disposed = false;
	const RENDER_BATCH = 200;

	const loadScope = new RequestScope();
	const folderScope = new RequestScope();
	const previewScope = new RequestScope();

	$: shownItems = listing ? listing.items.slice(0, renderLimit) : [];
	$: previewDisplay = (() => {
		if (previewState === 'loading') return 'Loading…';
		if (previewState === 'loaded') return previewText || 'Empty file.';
		if (previewState === 'too-large')
			return 'Preview unavailable: this file is larger than 2 MB. Download it to view the contents.';
		if (previewState === 'error') return 'Preview unavailable. Download the file to view it.';
		if (previewState === 'empty') return 'Empty file.';
		return 'No text preview.';
	})();

	onMount(() => {
		disposed = false;
		token = $page.params.token ?? '';
		void load();
		return () => {
			disposed = true;
			loadScope.invalidate();
			folderScope.invalidate();
			previewScope.invalidate();
		};
	});

	function message(err: unknown): string {
		return err instanceof Error && err.message ? err.message : 'This link is not available.';
	}

	async function load() {
		const op = loadScope.begin();
		loading = true;
		errorMsg = '';
		try {
			const result = await api.publicShare(token, op.signal);
			if (!loadScope.isCurrent(op.id)) return;
			share = result;
			renderLimit = RENDER_BATCH;
			if (result.kind === 'file' && result.file) {
				selected = result.file;
				void loadTextPreview(result.file);
			} else if (result.folder) {
				listing = {
					folder: result.folder,
					breadcrumbs: result.breadcrumbs ?? [],
					items: result.items ?? [],
					truncated: result.truncated === true
				};
				truncated = result.truncated === true;
			}
			document.title = `${result.name} · ${result.instance_name}`;
		} catch (err) {
			if (isAbortError(err) || !loadScope.isCurrent(op.id)) return;
			errorMsg = message(err);
		} finally {
			if (loadScope.isCurrent(op.id)) loading = false;
		}
	}

	async function openFolder(id: string) {
		const op = folderScope.begin();
		errorMsg = '';
		selected = null;
		previewScope.invalidate();
		previewText = '';
		previewState = 'idle';
		loading = true;
		renderLimit = RENDER_BATCH;
		try {
			const result = await api.publicNodes(token, id, op.signal);
			if (!folderScope.isCurrent(op.id)) return;
			listing = result;
			truncated = result.truncated === true;
		} catch (err) {
			if (isAbortError(err) || !folderScope.isCurrent(op.id)) return;
			errorMsg = message(err);
		} finally {
			if (folderScope.isCurrent(op.id)) loading = false;
		}
	}

	function openFile(file: PublicFileNode) {
		folderScope.abort();
		selected = file;
		previewText = '';
		previewState = 'idle';
		void loadTextPreview(file);
	}

	async function loadTextPreview(file: PublicFileNode) {
		const op = previewScope.begin();
		if (!isTexty(file.mime)) return;
		if (file.size > PREVIEW_MAX_BYTES) {
			previewState = 'too-large';
			return;
		}
		if (file.size === 0) {
			previewState = 'empty';
			return;
		}
		previewState = 'loading';
		try {
			const res = await fetch(publicContentUrl(token, file.id), {
				headers: { Range: `bytes=0-${PREVIEW_PREFIX_BYTES - 1}` },
				signal: op.signal
			});
			if (!res.ok) throw new Error(`Preview failed (${res.status})`);
			const { text, truncated } = await readTextPrefix(res, PREVIEW_PREFIX_BYTES);
			if (!previewScope.isCurrent(op.id) || selected?.id !== file.id) return;
			const limited = truncateCodePoints(text, 120_000);
			previewText = limited.truncated || truncated ? `${limited.text}\n… (truncated)` : text;
			previewState = previewText ? 'loaded' : 'empty';
		} catch (err) {
			if (isAbortError(err) || !previewScope.isCurrent(op.id)) return;
			previewState = 'error';
		}
	}

	function closePreview() {
		previewScope.invalidate();
		selected = null;
		previewText = '';
		previewState = 'idle';
	}

	function formatBytes(bytes: number): string {
		if (!bytes) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
		const value = bytes / Math.pow(1024, index);
		return `${value.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
	}

	function formatDate(ts?: number | null): string {
		if (!ts) return '';
		return new Date(ts * 1000).toLocaleString(undefined, {
			dateStyle: 'medium',
			timeStyle: 'short'
		});
	}

	function extensionOf(name: string): string {
		const dot = name.lastIndexOf('.');
		if (dot <= 0 || dot === name.length - 1) return 'FILE';
		return name.slice(dot + 1).toUpperCase().slice(0, 4);
	}

	function isImage(mime?: string | null): boolean {
		return !!mime && mime.startsWith('image/') && mime !== 'image/svg+xml';
	}

	function isPdf(mime?: string | null): boolean {
		return mime === 'application/pdf';
	}

	function isVideo(mime?: string | null): boolean {
		return !!mime && mime.startsWith('video/');
	}

	function isAudio(mime?: string | null): boolean {
		return !!mime && mime.startsWith('audio/');
	}

	function isTexty(mime?: string | null): boolean {
		if (!mime) return false;
		return (
			mime.startsWith('text/') ||
			mime === 'application/json' ||
			mime === 'application/xml' ||
			mime === 'application/javascript'
		);
	}

	function isCurrentCrumb(index: number): boolean {
		return !!listing && index === listing.breadcrumbs.length - 1;
	}
</script>

<svelte:head>
	<title>{share ? `${share.name} · ${share.instance_name}` : 'Shared link · Nimbo'}</title>
</svelte:head>

<div class="public-shell">
	<header class="public-topbar">
		<div class="brand">
			<span class="mark">N</span>
			<span class="name">{share?.instance_name ?? 'Nimbo'}</span>
		</div>
		<div class="instance">Shared with you</div>
		<div class="spacer"></div>
		{#if share?.expires_at}
			<span class="expiry">Link expires {formatDate(share.expires_at)}</span>
		{/if}
		<a class="btn ghost" href="/">Open Nimbo</a>
	</header>

	<main class="public-main">
		{#if loading}
			<div class="center"><div class="spinner"></div></div>
		{:else if errorMsg}
			<div class="empty-state">
				<div class="inner">
					<div class="brand" style="justify-content: center;">
						<span class="mark">N</span>
						<span class="name">Nimbo</span>
					</div>
					<h2>This link is not available</h2>
					<p>{errorMsg}</p>
					<p class="hint">The owner may have revoked it, or it may have reached its expiry date.</p>
				</div>
			</div>
		{:else if share}
			<div class="public-toolbar">
				{#if share.kind === 'folder' && listing}
					<nav class="crumbs">
						<button
							class="crumb"
							class:current={listing.breadcrumbs.length === 0}
							on:click={() => openFolder(share!.folder!.id)}
						>
							{share.name}
						</button>
						{#each listing.breadcrumbs as crumb, index (crumb.id)}
							<span class="crumb-sep">/</span>
							<button
								class="crumb"
								class:current={isCurrentCrumb(index)}
								on:click={() => openFolder(crumb.id)}
							>
								{crumb.name}
							</button>
						{/each}
					</nav>
					<div class="spacer"></div>
					<a class="btn" href={publicZipUrl(token, listing.folder.id)}>Download .zip</a>
				{:else}
					<span class="file-heading">{share.name}</span>
					<div class="spacer"></div>
					{#if share.file}
						<a
							class="btn primary"
							href={publicDownloadUrl(token, share.file.id)}
							download={share.file.name}
						>
							Download
						</a>
					{/if}
				{/if}
			</div>

			{#if share.kind === 'file' && share.file}
				<div class="single-file">
					<div class="public-preview large">
						{#if isImage(share.file.mime)}
							<img
								src={publicContentUrl(token, share.file.id)}
								alt={share.file.name}
								class="preview-image"
							/>
						{:else if isPdf(share.file.mime)}
							<iframe
								class="preview-frame"
								src={publicContentUrl(token, share.file.id)}
								title={share.file.name}
							></iframe>
						{:else if isVideo(share.file.mime)}
							<!-- svelte-ignore a11y_media_has_caption -->
							<video
								class="preview-video"
								controls
								src={publicContentUrl(token, share.file.id)}
							></video>
						{:else if isAudio(share.file.mime)}
							<audio
								class="preview-audio"
								controls
								src={publicContentUrl(token, share.file.id)}
							></audio>
						{:else if isTexty(share.file.mime)}
							<pre class="preview-text">{previewDisplay}</pre>
						{:else}
							<div class="preview-none">
								<span class="badge big">{extensionOf(share.file.name)}</span>
								<span>No inline preview for this file type.</span>
							</div>
						{/if}
					</div>
					<div class="file-meta">
						<span>{share.file.mime || 'File'}</span>
						<span>{formatBytes(share.file.size)}</span>
					</div>
				</div>
			{:else if share.kind === 'folder' && listing}
				<div class="folder-layout" class:with-preview={!!selected}>
					<div class="folder-list">
						{#if listing.items.length === 0}
							<div class="empty-folder">This folder is empty.</div>
						{:else}
							<div class="card-grid">
								{#each shownItems as item (item.id)}
									<div class="public-card" class:selected={selected?.id === item.id}>
										<button
											class="card-main"
											on:click={() => (item.kind === 'folder' ? openFolder(item.id) : openFile(item))}
											title={item.name}
										>
											<span class="thumb">
												{#if item.kind === 'folder'}
													<span class="folder-glyph"></span>
												{:else if isImage(item.mime) && item.has_thumb}
													<img src={publicThumbUrl(token, item.id)} alt="" loading="lazy" />
												{:else}
													<span class="badge big">{extensionOf(item.name)}</span>
												{/if}
											</span>
											<span class="card-name">{item.name}</span>
											<span class="card-meta">
												{item.kind === 'folder' ? 'Folder' : formatBytes(item.size)}
											</span>
										</button>
										{#if item.kind === 'file'}
											<a
												class="mini-download"
												href={publicDownloadUrl(token, item.id)}
												download={item.name}
												title="Download"
												on:click|stopPropagation
											>
												↓
											</a>
										{/if}
									</div>
								{/each}
							</div>
							{#if listing.items.length > shownItems.length}
								<div class="load-more">
									<span>Showing {shownItems.length} of {listing.items.length} items</span>
									<button
										class="btn"
										on:click={() => (renderLimit = Math.min(listing!.items.length, renderLimit + RENDER_BATCH))}
									>
										Show more
									</button>
								</div>
							{:else if truncated}
								<div class="load-more">
									<span>
										Showing the first {listing.items.length} items. The folder contains more
										than the public listing limit; download the folder to see everything.
									</span>
								</div>
							{/if}
						{/if}
					</div>

					{#if selected}
						<aside class="public-details">
							<div class="details-head">
								<span class="details-title" title={selected.name}>{selected.name}</span>
								<button class="btn ghost" on:click={closePreview}>✕</button>
							</div>
							<div class="details-body">
								<div class="public-preview">
									{#if isImage(selected.mime)}
										<img
											src={publicContentUrl(token, selected.id)}
											alt={selected.name}
											class="preview-image"
										/>
									{:else if isPdf(selected.mime)}
										<iframe
											class="preview-frame"
											src={publicContentUrl(token, selected.id)}
											title={selected.name}
										></iframe>
									{:else if isVideo(selected.mime)}
										<!-- svelte-ignore a11y_media_has_caption -->
										<video
											class="preview-video"
											controls
											src={publicContentUrl(token, selected.id)}
										></video>
									{:else if isAudio(selected.mime)}
										<audio
											class="preview-audio"
											controls
											src={publicContentUrl(token, selected.id)}
										></audio>
									{:else if isTexty(selected.mime)}
										<pre class="preview-text">{previewDisplay}</pre>
									{:else}
										<div class="preview-none">
											<span class="badge big">{extensionOf(selected.name)}</span>
											<span>No inline preview for this file type.</span>
										</div>
									{/if}
								</div>
								<div class="details-meta">
									<span>{selected.mime || 'File'}</span>
									<span>{formatBytes(selected.size)}</span>
								</div>
								<a
									class="btn primary block"
									href={publicDownloadUrl(token, selected.id)}
									download={selected.name}
								>
									Download
								</a>
							</div>
						</aside>
					{/if}
				</div>
			{/if}
		{/if}
	</main>
</div>

<style>
	.public-shell {
		min-height: 100vh;
		display: flex;
		flex-direction: column;
	}
	.public-topbar {
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 0 18px;
		height: 56px;
		border-bottom: 1px solid var(--line-soft);
		background: rgba(8, 9, 12, 0.72);
		backdrop-filter: blur(10px);
		position: sticky;
		top: 0;
		z-index: 10;
	}
	.public-topbar .instance {
		color: var(--muted);
		font-size: 13px;
		padding-left: 12px;
		border-left: 1px solid var(--line);
	}
	.public-topbar .spacer {
		flex: 1;
	}
	.expiry {
		font-size: 12px;
		color: var(--faint);
	}
	.public-main {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
		width: 100%;
		max-width: 1280px;
		margin: 0 auto;
		padding: 0 18px 30px;
	}
	.public-toolbar {
		display: flex;
		align-items: center;
		gap: 10px;
		min-height: 60px;
		padding: 10px 0;
		border-bottom: 1px solid var(--line-soft);
		flex-wrap: wrap;
	}
	.public-toolbar .spacer {
		flex: 1;
	}
	.file-heading {
		font-size: 15px;
		font-weight: 650;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 70%;
	}
	.crumbs {
		display: flex;
		align-items: center;
		gap: 4px;
		overflow: hidden;
		min-width: 0;
	}
	.crumb {
		border: 0;
		background: transparent;
		color: var(--muted);
		font-size: 13px;
		padding: 4px 6px;
		border-radius: 6px;
		cursor: pointer;
		white-space: nowrap;
	}
	.crumb:hover {
		background: var(--panel-2);
		color: var(--text);
	}
	.crumb.current {
		color: var(--text);
		font-weight: 600;
	}
	.crumb-sep {
		color: var(--faint);
		font-size: 12px;
	}

	.single-file {
		max-width: 920px;
		width: 100%;
		margin: 26px auto 0;
	}
	.public-preview {
		border: 1px solid var(--line-soft);
		border-radius: 12px;
		overflow: hidden;
		background: var(--bg-2);
	}
	.public-preview.large {
		min-height: 220px;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.preview-image {
		display: block;
		width: 100%;
		height: min(68vh, 640px);
		object-fit: contain;
		margin: 0 auto;
		background: #05060a;
	}
	.public-details .preview-image {
		height: 260px;
	}
	.preview-frame {
		width: 100%;
		height: 68vh;
		border: 0;
		background: #fff;
	}
	.preview-video {
		width: 100%;
		max-height: 68vh;
		display: block;
		background: #05060a;
	}
	.preview-audio {
		width: 100%;
		padding: 18px;
	}
	.preview-text {
		margin: 0;
		padding: 16px;
		width: 100%;
		max-height: 68vh;
		overflow: auto;
		font-family: var(--mono);
		font-size: 12.5px;
		line-height: 1.55;
		white-space: pre-wrap;
		word-break: break-word;
		color: #c8d2e6;
	}
	.preview-none {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
		padding: 46px 20px;
		color: var(--faint);
		font-size: 12.5px;
	}
	.file-meta,
	.details-meta {
		display: flex;
		gap: 14px;
		padding: 10px 2px;
		font-size: 12px;
		color: var(--faint);
	}

	.folder-layout {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 18px;
		padding-top: 18px;
	}
	.folder-layout.with-preview {
		grid-template-columns: minmax(0, 1fr) 360px;
	}
	.card-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
		gap: 12px;
	}
	.public-card {
		position: relative;
		border: 1px solid var(--line-soft);
		background: rgba(17, 20, 26, 0.65);
		border-radius: 12px;
		overflow: hidden;
		transition: border-color 0.15s, background 0.15s;
	}
	.public-card:hover {
		border-color: var(--line);
		background: var(--panel-2);
	}
	.public-card.selected {
		border-color: rgba(138, 180, 255, 0.55);
	}
	.card-main {
		display: flex;
		flex-direction: column;
		width: 100%;
		border: 0;
		background: transparent;
		padding: 11px;
		text-align: left;
		cursor: pointer;
		color: var(--text);
		gap: 5px;
	}
	.thumb {
		height: 92px;
		border-radius: 8px;
		background: var(--bg-2);
		border: 1px solid var(--line-soft);
		display: grid;
		place-items: center;
		overflow: hidden;
		margin-bottom: 3px;
	}
	.thumb img {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}
	.folder-glyph {
		width: 48px;
		height: 36px;
		border-radius: 5px;
		background: linear-gradient(180deg, rgba(138, 180, 255, 0.78), rgba(138, 180, 255, 0.38));
		position: relative;
	}
	.folder-glyph::before {
		content: '';
		position: absolute;
		top: -6px;
		left: 0;
		width: 20px;
		height: 8px;
		border-radius: 4px 4px 0 0;
		background: rgba(138, 180, 255, 0.7);
	}
	.badge {
		display: inline-grid;
		place-items: center;
		min-width: 42px;
		height: 26px;
		padding: 0 6px;
		border-radius: 6px;
		background: rgba(138, 180, 255, 0.1);
		border: 1px solid rgba(138, 180, 255, 0.18);
		color: #cfe0ff;
		font-family: var(--mono);
		font-size: 10px;
		font-weight: 700;
	}
	.badge.big {
		min-width: 54px;
		height: 46px;
		font-size: 12px;
	}
	.card-name {
		font-size: 13px;
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.card-meta {
		font-size: 11px;
		color: var(--faint);
	}
	.mini-download {
		position: absolute;
		top: 7px;
		right: 7px;
		width: 26px;
		height: 26px;
		display: grid;
		place-items: center;
		border-radius: 7px;
		border: 1px solid var(--line-soft);
		background: rgba(8, 9, 12, 0.82);
		color: var(--muted);
		text-decoration: none;
		font-size: 13px;
		opacity: 0;
		transition: opacity 0.12s;
	}
	.public-card:hover .mini-download {
		opacity: 1;
	}
	.mini-download:hover {
		color: var(--text);
	}

	.public-details {
		border: 1px solid var(--line-soft);
		border-radius: 12px;
		background: rgba(13, 16, 21, 0.6);
		overflow: hidden;
		height: fit-content;
		position: sticky;
		top: 76px;
	}
	.details-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 10px 12px;
		border-bottom: 1px solid var(--line-soft);
	}
	.details-title {
		flex: 1;
		font-size: 13px;
		font-weight: 650;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.details-body {
		padding: 12px;
	}
	.details-body .btn.block {
		margin-top: 8px;
	}
	.empty-folder {
		border: 1px dashed var(--line);
		border-radius: 12px;
		padding: 50px 20px;
		text-align: center;
		color: var(--faint);
		font-size: 13px;
	}
	.hint {
		color: var(--faint);
		font-size: 12.5px;
	}
	.empty-state .inner p {
		max-width: 460px;
		margin-left: auto;
		margin-right: auto;
	}

	@media (max-width: 980px) {
		.folder-layout.with-preview {
			grid-template-columns: minmax(0, 1fr);
		}
		.public-details {
			position: static;
		}
	}
	@media (max-width: 700px) {
		.public-main {
			padding: 0 12px 24px;
		}
		.public-topbar {
			padding: 0 12px;
			gap: 9px;
		}
		.public-topbar .expiry {
			display: none;
		}
		.card-grid {
			grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
		}
		.file-heading {
			max-width: 100%;
		}
	}
</style>
