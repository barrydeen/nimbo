<script lang="ts">
	import { onMount } from 'svelte';
	import { goto, replaceState } from '$app/navigation';
	import {
		api,
		fileContentUrl,
		fileDownloadUrl,
		fileThumbUrl,
		uploadFiles,
		type Breadcrumb,
		type FileDetails,
		type FileNode,
		type FilesStats,
		type ShareLink,
		type User
	} from '$lib/api';

	let user: User | null = null;
	let instanceName = 'Nimbo';
	let booted = false;

	let view: 'drive' | 'recent' | 'shares' = 'drive';
	let folderId: string | null = null;
	let breadcrumbs: Breadcrumb[] = [];
	let items: FileNode[] = [];
	let recent: FileNode[] = [];
	let shares: ShareLink[] = [];
	let stats: FilesStats | null = null;

	let selected: FileDetails | null = null;
	let selectedId: string | null = null;
	let previewText = '';
	let previewLoading = false;

	let loading = false;
	let errorMsg = '';
	let statusMsg = '';
	let statusTimer: ReturnType<typeof setTimeout> | null = null;

	let layout: 'grid' | 'list' = 'grid';
	let searchQuery = '';
	let searchResults: FileNode[] = [];
	let searchMode = false;
	let searching = false;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;

	let showNewFolder = false;
	let newFolderName = '';
	let creatingFolder = false;

	let fileInput: HTMLInputElement;
	let uploading = false;
	let uploadPercent = 0;
	let uploadLabel = '';
	let dragActive = false;
	let origin = '';

	let shareOpen = false;
	let shareTarget: FileNode | null = null;
	let shareExpiry: 'never' | '1h' | '1d' | '7d' | '30d' = 'never';
	let shareCreated: ShareLink | null = null;
	let shareExisting: ShareLink[] = [];
	let shareBusy = false;
	let shareError = '';

	$: metaRows = selected ? metadataRows(selected.item.metadata) : [];

	onMount(() => {
		(async () => {
			try {
				const boot = await api.bootstrap();
				if (!boot.setup_complete) {
					goto('/setup');
					return;
				}
				if (!boot.user) {
					goto('/login');
					return;
				}
				user = boot.user;
				instanceName = boot.instance_name;
				booted = true;
				origin = location.origin;
				const requested = new URLSearchParams(location.search).get('folder');
				await loadFolder(requested || null);
				await loadStats();
			} catch {
				goto('/login');
			}
		})();
	});

	function message(err: unknown): string {
		return err instanceof Error ? err.message : 'Something went wrong';
	}

	function flash(text: string) {
		statusMsg = text;
		if (statusTimer) clearTimeout(statusTimer);
		statusTimer = setTimeout(() => (statusMsg = ''), 2200);
	}

	async function loadStats() {
		try {
			stats = await api.filesStats();
		} catch {
			stats = null;
		}
	}

	async function loadFolder(id: string | null) {
		loading = true;
		errorMsg = '';
		searchMode = false;
		view = 'drive';
		selected = null;
		selectedId = null;
		previewText = '';
		try {
			const listing = await api.listFiles(id);
			folderId = listing.folder.id === 'root' ? null : listing.folder.id;
			breadcrumbs = listing.breadcrumbs;
			items = listing.items;
			replaceState(
				folderId ? `/drive?folder=${encodeURIComponent(folderId)}` : '/drive',
				{}
			);
		} catch (err) {
			errorMsg = message(err);
			if (id) {
				await loadFolder(null);
			}
		} finally {
			loading = false;
		}
	}

	async function loadRecent() {
		loading = true;
		errorMsg = '';
		selected = null;
		selectedId = null;
		previewText = '';
		view = 'recent';
		try {
			recent = (await api.recentFiles()).items;
		} catch (err) {
			errorMsg = message(err);
		} finally {
			loading = false;
		}
	}

	async function loadShares() {
		loading = true;
		errorMsg = '';
		selected = null;
		selectedId = null;
		previewText = '';
		view = 'shares';
		try {
			shares = (await api.listShares()).shares;
		} catch (err) {
			errorMsg = message(err);
		} finally {
			loading = false;
		}
	}

	async function refreshCurrent() {
		if (view === 'shares') {
			await loadShares();
		} else if (view === 'recent') {
			await loadRecent();
		} else {
			await loadFolder(folderId);
		}
		await loadStats();
	}

	function openItem(item: FileNode) {
		if (item.kind === 'folder') {
			loadFolder(item.id);
		} else {
			selectFile(item.id);
		}
	}

	async function selectFile(id: string) {
		selectedId = id;
		previewText = '';
		try {
			const details = await api.fileDetails(id);
			if (selectedId !== id) return;
			selected = details;
			if (isTexty(details.item.mime)) {
				loadTextPreview(id);
			}
		} catch (err) {
			selectedId = null;
			errorMsg = message(err);
		}
	}

	async function loadTextPreview(id: string) {
		previewLoading = true;
		try {
			const res = await fetch(fileContentUrl(id), { credentials: 'include' });
			if (!res.ok) return;
			const text = await res.text();
			if (selectedId === id) {
				previewText =
					text.length > 120_000 ? `${text.slice(0, 120_000)}\n… (truncated)` : text;
			}
		} catch {
			/* keep the extracted preview */
		} finally {
			previewLoading = false;
		}
	}

	function closeDetails() {
		selected = null;
		selectedId = null;
		previewText = '';
	}

	async function createFolder() {
		const name = newFolderName.trim();
		if (!name || creatingFolder) return;
		creatingFolder = true;
		errorMsg = '';
		try {
			await api.createFolder({ parent_id: folderId, name });
			newFolderName = '';
			showNewFolder = false;
			await loadFolder(folderId);
			await loadStats();
			flash('Folder created');
		} catch (err) {
			errorMsg = message(err);
		} finally {
			creatingFolder = false;
		}
	}

	async function renameItem(item: FileNode) {
		const next = prompt('New name', item.name);
		if (!next || next.trim() === item.name) return;
		try {
			await api.updateNode(item.id, { name: next.trim() });
			await refreshCurrent();
			if (selectedId === item.id) await selectFile(item.id);
			flash('Renamed');
		} catch (err) {
			errorMsg = message(err);
		}
	}

	async function deleteItem(item: FileNode) {
		const extra =
			item.kind === 'folder' ? ' Everything inside the folder will be removed.' : '';
		if (!confirm(`Delete “${item.name}”?${extra}`)) return;
		try {
			await api.deleteNode(item.id);
			if (selectedId === item.id) closeDetails();
			await refreshCurrent();
			await loadStats();
			flash('Deleted');
		} catch (err) {
			errorMsg = message(err);
		}
	}

	function pickFiles() {
		fileInput?.click();
	}

	function onFileInput(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		const files = Array.from(input.files ?? []);
		input.value = '';
		if (files.length) doUpload(files);
	}

	async function doUpload(files: File[]) {
		if (!files.length || uploading) return;
		uploading = true;
		uploadPercent = 0;
		uploadLabel =
			files.length === 1 ? files[0].name : `${files.length} files`;
		errorMsg = '';
		try {
			const result = await uploadFiles(folderId, files, (percent) => {
				uploadPercent = percent;
			});
			if (result.failed?.length) {
				errorMsg = result.failed.map((f) => `${f.name}: ${f.error}`).join(' · ');
			}
			const count = result.uploaded?.length ?? 0;
			if (count) flash(`Uploaded ${count} ${count === 1 ? 'file' : 'files'}`);
			if (view !== 'drive') {
				await loadFolder(folderId);
			} else {
				await loadFolder(folderId);
			}
			await loadStats();
		} catch (err) {
			errorMsg = message(err);
		} finally {
			uploading = false;
			uploadPercent = 0;
			uploadLabel = '';
		}
	}

	function onDragOver(event: DragEvent) {
		if (event.dataTransfer?.types.includes('Files')) {
			event.preventDefault();
			dragActive = true;
		}
	}

	function onDragLeave(event: DragEvent) {
		const related = event.relatedTarget as Node | null;
		if (!related || !(event.currentTarget as HTMLElement).contains(related)) {
			dragActive = false;
		}
	}

	function onDrop(event: DragEvent) {
		event.preventDefault();
		dragActive = false;
		const files = Array.from(event.dataTransfer?.files ?? []);
		if (files.length) doUpload(files);
	}

	function runSearch() {
		const q = searchQuery.trim();
		if (!q) {
			clearSearch();
			return;
		}
		searching = true;
		searchMode = true;
		errorMsg = '';
		api
			.searchFiles(q)
			.then((res) => {
				searchResults = res.results;
			})
			.catch((err) => {
				errorMsg = message(err);
			})
			.finally(() => {
				searching = false;
			});
	}

	function onSearchInput() {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(() => {
			if (searchQuery.trim()) runSearch();
			else clearSearch();
		}, 250);
	}

	function clearSearch() {
		searchQuery = '';
		searchResults = [];
		searchMode = false;
	}

	function openShare(item: FileNode) {
		shareTarget = item;
		shareCreated = null;
		shareExisting = [];
		shareExpiry = 'never';
		shareError = '';
		shareOpen = true;
		api
			.listShares()
			.then((res) => {
				shares = res.shares;
				shareExisting = res.shares.filter((share) => share.target_id === item.id);
			})
			.catch(() => {
				shareExisting = [];
			});
	}

	function expirySeconds(choice: string): number {
		switch (choice) {
			case '1h':
				return 3600;
			case '1d':
				return 86_400;
			case '7d':
				return 7 * 86_400;
			case '30d':
				return 30 * 86_400;
			default:
				return 0;
		}
	}

	async function createShareLink() {
		if (!shareTarget || shareBusy) return;
		shareBusy = true;
		shareError = '';
		try {
			const seconds = expirySeconds(shareExpiry);
			const res = await api.createShare({
				target_type: shareTarget.kind === 'folder' ? 'folder' : 'file',
				target_id: shareTarget.id,
				expires_at: seconds ? Math.floor(Date.now() / 1000) + seconds : null
			});
			shareCreated = res.share;
			shareExisting = [res.share, ...shareExisting];
			if (selected && selected.item.id === res.share.target_id) {
				selected = { ...selected, shares: [res.share, ...selected.shares] };
			}
			await loadStats();
			await copyShare(res.share);
		} catch (err) {
			shareError = message(err);
		} finally {
			shareBusy = false;
		}
	}

	async function copyShare(share: ShareLink) {
		const absolute = `${location.origin}/s/${share.token}`;
		try {
			await navigator.clipboard.writeText(absolute);
			flash('Share link copied');
		} catch {
			prompt('Copy this share link', absolute);
		}
	}

	async function revokeShare(share: ShareLink) {
		if (!confirm('Revoke this link? Anyone using it will lose access immediately.')) return;
		try {
			await api.revokeShare(share.id);
			shareExisting = shareExisting.map((s) =>
				s.id === share.id
					? ({ ...s, status: 'revoked', revoked_at: Math.floor(Date.now() / 1000) } as ShareLink)
					: s
			);
			if (selected) {
				selected = {
					...selected,
					shares: selected.shares.map((s) =>
						s.id === share.id
							? ({
									...s,
									status: 'revoked',
									revoked_at: Math.floor(Date.now() / 1000)
								} as ShareLink)
							: s
					)
				};
			}
			if (shareCreated?.id === share.id) {
				shareCreated = { ...shareCreated, status: 'revoked' } as ShareLink;
			}
			if (view === 'shares') await loadShares();
			await loadStats();
			flash('Link revoked');
		} catch (err) {
			shareError = message(err);
		}
	}

	async function changeShareExpiry(share: ShareLink) {
		const answer = prompt(
			'Expire in how many days? Enter 0 to remove the expiry.',
			share.expires_at ? '7' : '0'
		);
		if (answer === null) return;
		const days = Number(answer);
		if (!Number.isFinite(days) || days < 0) return;
		try {
			const res = await api.updateShare(
				share.id,
				days === 0
					? { clear_expiry: true }
					: { expires_at: Math.floor(Date.now() / 1000) + Math.round(days * 86_400) }
			);
			if (view === 'shares') await loadShares();
			else {
				shareExisting = shareExisting.map((s) => (s.id === share.id ? res.share : s));
			}
			if (selected) await selectFile(selected.item.id);
			flash('Expiry updated');
		} catch (err) {
			shareError = message(err);
		}
	}

	async function retryVision() {
		if (!selected) return;
		try {
			await api.retryVision(selected.item.id);
			flash('Image description queued');
			setTimeout(() => selected && selectFile(selected.item.id), 2500);
		} catch (err) {
			errorMsg = message(err);
		}
	}

	async function retryIndex() {
		if (!selected) return;
		try {
			await api.reindexFile(selected.item.id);
			flash('Reindexing queued');
			setTimeout(() => selected && selectFile(selected.item.id), 2500);
		} catch (err) {
			errorMsg = message(err);
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key !== 'Escape') return;
		if (shareOpen) {
			shareOpen = false;
		} else if (selected) {
			closeDetails();
		}
	}

	async function logout() {
		try {
			await api.logout();
		} finally {
			goto('/login');
		}
	}

	function formatBytes(bytes: number): string {
		if (!bytes) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
		const value = bytes / Math.pow(1024, index);
		return `${value.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
	}

	function formatDate(ts?: number | null): string {
		if (!ts) return '—';
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
			mime === 'application/javascript' ||
			mime === 'application/x-yaml' ||
			mime === 'application/toml'
		);
	}

	function statusLabel(status?: string): string {
		switch (status) {
			case 'done':
				return 'Ready';
			case 'partial':
				return 'Partial';
			case 'pending':
				return 'Queued';
			case 'running':
				return 'Working';
			case 'failed':
				return 'Failed';
			case 'skipped':
				return 'Skipped';
			case 'n/a':
				return '';
			default:
				return status ?? '';
		}
	}

	function statusClass(status?: string): string {
		if (status === 'done') return 'ok';
		if (status === 'failed') return 'bad';
		if (status === 'partial' || status === 'skipped') return 'warn';
		return 'idle';
	}

	function flattenMetadata(
		value: unknown,
		prefix = '',
		out: { key: string; value: string }[] = []
	): { key: string; value: string }[] {
		if (value === null || value === undefined) return out;
		if (Array.isArray(value)) {
			out.push({
				key: prefix,
				value: value
					.map((entry) => (typeof entry === 'object' ? JSON.stringify(entry) : String(entry)))
					.join(', ')
			});
			return out;
		}
		if (typeof value === 'object') {
			for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
				flattenMetadata(entry, prefix ? `${prefix} · ${key}` : key, out);
			}
			return out;
		}
		out.push({ key: prefix, value: String(value) });
		return out;
	}

	function metadataRows(metadata: Record<string, unknown>): { key: string; value: string }[] {
		return flattenMetadata(metadata)
			.filter((row) => row.value !== '')
			.slice(0, 150);
	}

	function detailsNode(): FileNode | null {
		return selected ? selected.item : null;
	}
</script>

<svelte:window on:keydown={handleKeydown} />

<div class="shell drive-shell" class:with-details={!!selected}>
	<header class="topbar">
		<div class="brand">
			<span class="mark">N</span>
			<span class="name">{instanceName}</span>
		</div>
		<div class="instance">Files</div>
		<div class="topnav">
			<button class="btn ghost" on:click={() => goto('/vault')}>Notes</button>
			<button class="btn ghost active" on:click={() => loadFolder(folderId)}>Files</button>
		</div>
		<div class="spacer"></div>
		{#if user}
			<div class="user-chip">
				<span class="avatar">{user.username.slice(0, 1).toUpperCase()}</span>
				<span>{user.username}</span>
			</div>
		{/if}
		{#if user?.role === 'admin'}
			<button class="btn ghost" on:click={() => goto('/settings')}>Settings</button>
		{/if}
		<button class="btn ghost" on:click={logout}>Sign out</button>
	</header>

	<aside class="sidebar">
		<div class="sidebar-head">
			<div class="searchbox">
				<input
					placeholder="Search files, text, images…"
					bind:value={searchQuery}
					on:input={onSearchInput}
					on:keydown={(event) => {
						if (event.key === 'Enter') runSearch();
						if (event.key === 'Escape') clearSearch();
					}}
				/>
			</div>
			<div class="drive-actions">
				<button class="btn primary" on:click={pickFiles} disabled={uploading}>
					{uploading ? `${uploadPercent}%` : 'Upload'}
				</button>
				<button
					class="btn"
					on:click={() => (showNewFolder = !showNewFolder)}
					disabled={view !== 'drive'}
				>
					New folder
				</button>
				<input
					class="hidden-input"
					type="file"
					multiple
					bind:this={fileInput}
					on:change={onFileInput}
				/>
			</div>
			{#if showNewFolder}
				<div class="new-folder">
					<input
						class="input"
						placeholder="Folder name"
						bind:value={newFolderName}
						on:keydown={(event) => {
							if (event.key === 'Enter') createFolder();
							if (event.key === 'Escape') showNewFolder = false;
						}}
					/>
					<button class="btn" on:click={createFolder} disabled={creatingFolder}>
						{creatingFolder ? '…' : 'Create'}
					</button>
				</div>
			{/if}
		</div>

		<div class="sidebar-section"><span>Browse</span></div>
		<nav class="drive-nav">
			<button
				class="nav-item"
				class:active={view === 'drive' && !searchMode}
				on:click={() => loadFolder(folderId)}
			>
				<span class="nav-glyph">▦</span> My Drive
			</button>
			<button class="nav-item" class:active={view === 'recent'} on:click={loadRecent}>
				<span class="nav-glyph">◷</span> Recent
			</button>
			<button class="nav-item" class:active={view === 'shares'} on:click={loadShares}>
				<span class="nav-glyph">↗</span> Shared links
			</button>
		</nav>

		<div class="sidebar-section"><span>Storage</span></div>
		{#if stats}
			<div class="storage">
				<div class="storage-row">
					<strong>{formatBytes(stats.bytes)}</strong>
					<span>stored uniquely</span>
				</div>
				<div class="storage-grid">
					<div><strong>{stats.files}</strong><span>files</span></div>
					<div><strong>{stats.folders}</strong><span>folders</span></div>
					<div><strong>{stats.images}</strong><span>images</span></div>
					<div><strong>{stats.active_shares}</strong><span>links</span></div>
				</div>
			</div>
		{:else}
			<div class="sidebar-note">Storage statistics unavailable.</div>
		{/if}

		<div class="sidebar-spacer"></div>
		<div class="sidebar-note">
			Drag files anywhere to upload. Images are described automatically when a vision model is
			configured.
		</div>
	</aside>

	<main
		class="main drive-main"
		class:drag-active={dragActive}
		on:dragover={onDragOver}
		on:dragleave={onDragLeave}
		on:drop={onDrop}
	>
		<div class="toolbar">
			{#if searchMode}
				<span class="path">Search: “{searchQuery}”</span>
				<span class="count">
					{searching
						? 'searching…'
						: `${searchResults.length} ${searchResults.length === 1 ? 'result' : 'results'}`}
				</span>
				<div class="spacer"></div>
				<button class="btn ghost" on:click={clearSearch}>Clear</button>
			{:else if view === 'drive'}
				<nav class="crumbs">
					{#each breadcrumbs as crumb, index (crumb.id)}
						{#if index > 0}<span class="crumb-sep">/</span>{/if}
						<button
							class="crumb"
							class:current={index === breadcrumbs.length - 1}
							on:click={() => loadFolder(crumb.id === 'root' ? null : crumb.id)}
						>
							{crumb.name}
						</button>
					{/each}
					{#if breadcrumbs.length === 0}
						<button class="crumb current" on:click={() => loadFolder(null)}>My Drive</button>
					{/if}
				</nav>
				<span class="count">{items.length} {items.length === 1 ? 'item' : 'items'}</span>
				<div class="spacer"></div>
				<button class="btn ghost" on:click={pickFiles} disabled={uploading}>Upload</button>
			{:else if view === 'recent'}
				<span class="path">Recent files</span>
				<span class="count">{recent.length}</span>
				<div class="spacer"></div>
			{:else}
				<span class="path">Shared links</span>
				<span class="count">{shares.length}</span>
				<div class="spacer"></div>
			{/if}
			<div class="layout-toggle">
				<button
					class="btn ghost"
					class:active={layout === 'grid'}
					title="Grid view"
					on:click={() => (layout = 'grid')}>Grid</button
				>
				<button
					class="btn ghost"
					class:active={layout === 'list'}
					title="List view"
					on:click={() => (layout = 'list')}>List</button
				>
			</div>
			<button class="btn ghost" title="Refresh" on:click={refreshCurrent}>Refresh</button>
		</div>

		{#if uploading}
			<div class="upload-banner">
				<div class="upload-copy">
					<span>Uploading {uploadLabel}</span><span>{uploadPercent}%</span>
				</div>
				<div class="upload-track"><span style={`width:${uploadPercent}%`}></span></div>
			</div>
		{/if}

		{#if errorMsg}
			<div class="banner dismissable">
				<span>{errorMsg}</span>
				<button class="btn ghost" on:click={() => (errorMsg = '')}>Dismiss</button>
			</div>
		{/if}
		{#if statusMsg}
			<div class="toast">{statusMsg}</div>
		{/if}

		{#if dragActive}
			<div class="drop-overlay">
				<div class="drop-card">
					<strong>Drop files to upload</strong>
					<span>They go into the folder you are viewing.</span>
				</div>
			</div>
		{/if}

		{#if !booted || loading}
			<div class="center"><div class="spinner"></div></div>
		{:else if searchMode}
			<div class="content-scroll">
				{#if !searching && searchResults.length === 0}
					<div class="empty-card">
						<h2>No matches</h2>
						<p>Try searching for an image description, a phrase from a document, EXIF data, or a file name.</p>
					</div>
				{:else}
					<div class="result-list">
						{#each searchResults as result (result.id)}
							<button class="result-row" on:click={() => openItem(result)}>
								<span class="badge">{result.kind === 'folder' ? 'DIR' : extensionOf(result.name)}</span>
								<span class="result-main">
									<span class="result-name">{result.name}</span>
									<span class="result-path">{result.path || '—'}</span>
									{#if result.snippet}
										<span class="result-snippet">{@html result.snippet}</span>
									{/if}
								</span>
								<span class="result-size">{result.kind === 'file' ? formatBytes(result.size) : ''}</span>
							</button>
						{/each}
					</div>
				{/if}
			</div>
		{:else if view === 'drive'}
			<div class="content-scroll">
				{#if items.length === 0}
					<div class="empty-card">
						<h2>This folder is empty</h2>
						<p>Upload files or create a folder. Drag and drop works anywhere in this view.</p>
						<div class="empty-actions">
							<button class="btn primary" on:click={pickFiles}>Upload files</button>
							<button class="btn" on:click={() => (showNewFolder = true)}>New folder</button>
						</div>
					</div>
				{:else if layout === 'grid'}
					<div class="card-grid">
						{#each items as item (item.id)}
							<div class="file-card" class:selected={selectedId === item.id}>
								<button class="card-main" on:click={() => openItem(item)} title={item.name}>
									<span class="thumb">
										{#if item.kind === 'folder'}
											<span class="folder-glyph"></span>
										{:else if isImage(item.mime) && item.has_thumb}
											<img src={fileThumbUrl(item.id)} alt="" loading="lazy" />
										{:else}
											<span class="badge big">{extensionOf(item.name)}</span>
										{/if}
									</span>
									<span class="card-name">{item.name}</span>
									<span class="card-meta">
										{item.kind === 'folder'
											? 'Folder'
											: `${formatBytes(item.size)} · ${formatDate(item.updated_at)}`}
									</span>
									{#if item.kind === 'file' && item.index_status && item.index_status !== 'done' && item.index_status !== 'n/a'}
										<span class="mini-status {statusClass(item.index_status)}">{statusLabel(item.index_status)}</span>
									{/if}
								</button>
								<div class="card-actions">
									<button class="mini-btn" title="Share" on:click={() => openShare(item)}>↗</button>
									<button class="mini-btn" title="Rename" on:click={() => renameItem(item)}>✎</button>
									<button class="mini-btn danger" title="Delete" on:click={() => deleteItem(item)}>×</button>
								</div>
							</div>
						{/each}
					</div>
				{:else}
					<div class="row-list">
						<div class="row row-head">
							<span>Name</span><span>Size</span><span>Modified</span><span></span>
						</div>
						{#each items as item (item.id)}
							<div class="row" class:selected={selectedId === item.id}>
								<button class="row-main" on:click={() => openItem(item)}>
									<span class="badge">{item.kind === 'folder' ? 'DIR' : extensionOf(item.name)}</span>
									<span class="row-name">{item.name}</span>
								</button>
								<span class="row-size">{item.kind === 'file' ? formatBytes(item.size) : '—'}</span>
								<span class="row-date">{formatDate(item.updated_at)}</span>
								<span class="row-actions">
									<button class="mini-btn" title="Share" on:click={() => openShare(item)}>↗</button>
									<button class="mini-btn" title="Rename" on:click={() => renameItem(item)}>✎</button>
									<button class="mini-btn danger" title="Delete" on:click={() => deleteItem(item)}>×</button>
								</span>
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{:else if view === 'recent'}
			<div class="content-scroll">
				{#if recent.length === 0}
					<div class="empty-card"><h2>Nothing recent</h2><p>Uploaded files will appear here.</p></div>
				{:else}
					<div class="result-list">
						{#each recent as item (item.id)}
							<button class="result-row" on:click={() => selectFile(item.id)}>
								<span class="badge">{extensionOf(item.name)}</span>
								<span class="result-main">
									<span class="result-name">{item.name}</span>
									<span class="result-path">{item.path || '—'}</span>
								</span>
								<span class="result-size">{formatBytes(item.size)}</span>
								<span class="result-date">{formatDate(item.updated_at)}</span>
							</button>
						{/each}
					</div>
				{/if}
			</div>
		{:else}
			<div class="content-scroll">
				{#if shares.length === 0}
					<div class="empty-card">
						<h2>No shared links</h2>
						<p>Select a file or folder and choose Share to publish a revocable, optionally expiring link.</p>
					</div>
				{:else}
					<div class="result-list">
						{#each shares as share (share.id)}
							<div class="share-row">
								<span class="badge">{share.target_type === 'folder' ? 'DIR' : 'FILE'}</span>
								<span class="result-main">
									<span class="result-name">
										{share.target_name || 'Missing item'}
										<span class="pill {share.status}">{share.status}</span>
									</span>
									<span class="result-path">
										{share.target_path || '—'} · created {formatDate(share.created_at)}
										{#if share.expires_at}· expires {formatDate(share.expires_at)}{/if}
										· {share.access_count} visits
									</span>
								</span>
								<span class="share-actions">
									{#if share.status === 'active'}
										<button class="btn ghost" on:click={() => copyShare(share)}>Copy link</button>
										<a class="btn ghost" href={`/s/${share.token}`} target="_blank" rel="noreferrer">Open</a>
										<button class="btn ghost" on:click={() => changeShareExpiry(share)}>Expiry</button>
										<button class="btn danger" on:click={() => revokeShare(share)}>Revoke</button>
									{:else}
										<span class="muted-note">
											{share.status === 'expired' ? 'Link expired' : 'Link revoked'}
										</span>
									{/if}
								</span>
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</main>

	{#if selected}
		<aside class="details">
			<div class="details-head">
				<div class="details-title" title={selected.item.name}>{selected.item.name}</div>
				<button class="btn ghost" title="Close details" on:click={closeDetails}>✕</button>
			</div>
			<div class="details-body">
				<div class="preview-box">
					{#if isImage(selected.item.mime)}
						<img
							class="preview-image"
							src={fileContentUrl(selected.item.id)}
							alt={selected.item.name}
						/>
					{:else if isPdf(selected.item.mime)}
						<iframe
							class="preview-frame"
							src={fileContentUrl(selected.item.id)}
							title={selected.item.name}
						></iframe>
					{:else if isVideo(selected.item.mime)}
						<!-- svelte-ignore a11y_media_has_caption -->
						<video class="preview-video" controls src={fileContentUrl(selected.item.id)}></video>
					{:else if isAudio(selected.item.mime)}
						<audio class="preview-audio" controls src={fileContentUrl(selected.item.id)}></audio>
					{:else if isTexty(selected.item.mime)}
						<pre class="preview-text">{previewLoading ? 'Loading…' : previewText || selected.item.text_preview || 'No text preview.'}</pre>
					{:else}
						<div class="preview-none">
							<span class="badge big">{extensionOf(selected.item.name)}</span>
							<span>No inline preview for this type.</span>
						</div>
					{/if}
				</div>

				<div class="detail-actions">
					<a class="btn primary" href={fileDownloadUrl(selected.item.id)} download={selected.item.name}>Download</a>
					<button class="btn" on:click={() => { const node = detailsNode(); if (node) openShare(node); }}>Share</button>
					<button class="btn" on:click={() => { const node = detailsNode(); if (node) renameItem(node); }}>Rename</button>
					<button class="btn danger" on:click={() => { const node = detailsNode(); if (node) deleteItem(node); }}>Delete</button>
				</div>

				<div class="detail-section">
					<h3>Details</h3>
					<dl class="kv">
						<dt>Type</dt><dd>{selected.item.mime || selected.item.kind}</dd>
						<dt>Size</dt><dd>{formatBytes(selected.item.size)}</dd>
						<dt>Path</dt><dd class="mono">{selected.path || '—'}</dd>
						<dt>Modified</dt><dd>{formatDate(selected.item.updated_at)}</dd>
						<dt>SHA-256</dt><dd class="mono hash">{selected.item.sha256 || '—'}</dd>
						<dt>Index</dt>
						<dd>
							<span class="pill {statusClass(selected.item.index_status)}">
								{statusLabel(selected.item.index_status) || '—'}
							</span>
							{#if selected.item.index_status === 'partial' || selected.item.index_status === 'failed'}
								<button class="link-btn" on:click={retryIndex}>Reindex</button>
							{/if}
						</dd>
					</dl>
					{#if selected.item.index_error}
						<p class="detail-error">{selected.item.index_error}</p>
					{/if}
				</div>

				{#if isImage(selected.item.mime) || selected.item.vision.description || selected.item.vision.error}
				<div class="detail-section">
					<h3>
						Image description
						<span class="pill {statusClass(selected.item.vision.status)}">
							{statusLabel(selected.item.vision.status) || 'not an image'}
						</span>
					</h3>
					{#if selected.item.vision.description}
						<p class="vision-text">{selected.item.vision.description}</p>
					{/if}
					{#if selected.item.vision.text}
						<div class="vision-ocr">
							<span class="label">Visible text</span>
							<pre>{selected.item.vision.text}</pre>
						</div>
					{/if}
					{#if selected.item.vision.tags.length}
						<div class="tags">
							{#each selected.item.vision.tags as tag}
								<span class="tag">{tag}</span>
							{/each}
						</div>
					{/if}
					{#if selected.item.vision.error}
						<p class="detail-error">{selected.item.vision.error}</p>
					{/if}
					{#if selected.item.vision.model}
						<p class="detail-note">
							Described by {selected.item.vision.model} · {formatDate(selected.item.vision.at)}
						</p>
					{/if}
					{#if isImage(selected.item.mime) && ['failed', 'skipped'].includes(selected.item.vision.status)}
						<button class="btn" on:click={retryVision}>Retry description</button>
						{#if selected.item.vision.status === 'skipped'}
							<p class="detail-note">
								Configure a vision-capable model in settings to describe images automatically.
							</p>
						{/if}
					{/if}
				</div>
				{/if}

				{#if selected.item.text_preview}
					<div class="detail-section">
						<h3>Extracted text</h3>
						<details class="extracted">
							<summary>{selected.item.text_preview.split(/\s+/).length} words indexed</summary>
							<pre>{selected.item.text_preview}</pre>
						</details>
					</div>
				{/if}

				{#if metaRows.length}
					<div class="detail-section">
						<h3>Metadata</h3>
						<dl class="kv metadata">
							{#each metaRows as row (row.key)}
								<dt title={row.key}>{row.key}</dt>
								<dd title={row.value}>{row.value}</dd>
							{/each}
						</dl>
					</div>
				{/if}

				<div class="detail-section">
					<h3>Share links</h3>
					{#if selected.shares.length === 0}
						<p class="detail-note">This file is private. Create a link to share it publicly.</p>
					{:else}
						<div class="node-shares">
							{#each selected.shares as share (share.id)}
								<div class="node-share">
									<span class="pill {share.status}">{share.status}</span>
									<span class="node-share-expiry">
										{share.expires_at ? formatDate(share.expires_at) : 'No expiry'} · {share.access_count} visits
									</span>
									{#if share.status === 'active'}
										<button class="link-btn" on:click={() => copyShare(share)}>Copy</button>
										<button class="link-btn danger" on:click={() => revokeShare(share)}>Revoke</button>
									{/if}
								</div>
							{/each}
						</div>
					{/if}
					<button class="btn" on:click={() => detailsNode() && openShare(detailsNode()!)}>New share link</button>
				</div>
			</div>
		</aside>
	{/if}
</div>

{#if shareOpen && shareTarget}
	<div
		class="modal-backdrop"
		role="presentation"
		on:click={(event) => {
			if (event.currentTarget === event.target) shareOpen = false;
		}}
	>
		<div class="modal" role="dialog" aria-modal="true" aria-label="Share">
			<div class="modal-head">
				<div>
					<h2>Share “{shareTarget.name}”</h2>
					<p class="detail-note">
						Anyone with the link can {shareTarget.kind === 'folder' ? 'browse and download this folder' : 'download this file'}.
						Search metadata, extracted text, and image descriptions stay private.
					</p>
				</div>
				<button class="btn ghost" on:click={() => (shareOpen = false)}>✕</button>
			</div>

			{#if shareError}
				<div class="banner">{shareError}</div>
			{/if}

			<div class="share-create">
				<label class="label" for="expiry">Link expiry</label>
				<select id="expiry" class="input" bind:value={shareExpiry}>
					<option value="never">Never expires</option>
					<option value="1h">1 hour</option>
					<option value="1d">1 day</option>
					<option value="7d">7 days</option>
					<option value="30d">30 days</option>
				</select>
				<button class="btn primary" on:click={createShareLink} disabled={shareBusy}>
					{shareBusy ? 'Creating…' : 'Create link'}
				</button>
			</div>

			{#if shareCreated}
				<div class="created-link">
					<span class="label">Public link</span>
					<div class="link-row">
						<input
							class="input mono"
							readonly
							value={`${origin}/s/${shareCreated.token}`}
						/>
						<button class="btn" on:click={() => copyShare(shareCreated!)}>Copy</button>
					</div>
				</div>
			{/if}

			{#if shareExisting.length}
				<div class="share-existing">
					<span class="label">Existing links</span>
					{#each shareExisting as share (share.id)}
						<div class="share-existing-row">
							<span class="pill {share.status}">{share.status}</span>
							<span class="share-existing-meta">
								{share.expires_at ? `Expires ${formatDate(share.expires_at)}` : 'No expiry'}
								· {share.access_count} visits
							</span>
							{#if share.status === 'active'}
								<button class="link-btn" on:click={() => copyShare(share)}>Copy</button>
								<button class="link-btn" on:click={() => changeShareExpiry(share)}>Expiry</button>
								<button class="link-btn danger" on:click={() => revokeShare(share)}>Revoke</button>
							{/if}
						</div>
					{/each}
				</div>
			{/if}

			<div class="modal-foot">
				<button class="btn ghost" on:click={() => (shareOpen = false)}>Close</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.drive-shell {
		grid-template-columns: 270px minmax(0, 1fr);
	}
	.drive-shell.with-details {
		grid-template-columns: 270px minmax(0, 1fr) 360px;
	}

	.topnav {
		display: flex;
		gap: 4px;
		margin-left: 4px;
	}

	.drive-actions {
		display: flex;
		gap: 8px;
	}
	.drive-actions .btn {
		flex: 1;
	}
	.hidden-input {
		display: none;
	}

	.new-folder {
		display: flex;
		gap: 6px;
	}
	.new-folder .input {
		padding: 8px 10px;
		font-size: 13px;
	}

	.drive-nav {
		display: flex;
		flex-direction: column;
		padding: 0 8px;
		gap: 2px;
	}
	.nav-item {
		display: flex;
		align-items: center;
		gap: 9px;
		width: 100%;
		text-align: left;
		border: 1px solid transparent;
		background: transparent;
		color: var(--muted);
		border-radius: 9px;
		padding: 8px 10px;
		font-size: 13.5px;
		cursor: pointer;
	}
	.nav-item:hover {
		background: var(--panel-2);
		color: var(--text);
	}
	.nav-item.active {
		background: rgba(138, 180, 255, 0.1);
		border-color: rgba(138, 180, 255, 0.25);
		color: #dce8ff;
	}
	.nav-glyph {
		width: 16px;
		text-align: center;
		opacity: 0.9;
	}

	.storage {
		padding: 2px 14px 8px;
	}
	.storage-row {
		display: flex;
		justify-content: space-between;
		font-size: 12.5px;
		color: var(--muted);
		margin-bottom: 8px;
	}
	.storage-row strong {
		color: var(--text);
	}
	.storage-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 6px;
	}
	.storage-grid div {
		background: var(--bg-2);
		border: 1px solid var(--line-soft);
		border-radius: 8px;
		padding: 7px 9px;
		display: flex;
		justify-content: space-between;
		font-size: 12px;
	}
	.storage-grid strong {
		color: var(--text);
	}
	.storage-grid span {
		color: var(--faint);
	}
	.sidebar-spacer {
		flex: 1;
	}
	.sidebar-note {
		padding: 12px 14px 16px;
		font-size: 11.5px;
		line-height: 1.5;
		color: var(--faint);
	}

	.drive-main {
		position: relative;
		overflow: hidden;
	}
	.count {
		font-size: 12px;
		color: var(--faint);
		white-space: nowrap;
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
	.layout-toggle {
		display: flex;
		gap: 2px;
	}
	.layout-toggle .btn {
		padding: 6px 8px;
		font-size: 12px;
	}

	.content-scroll {
		flex: 1;
		overflow: auto;
		padding: 16px;
		min-height: 0;
	}

	.card-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(168px, 1fr));
		gap: 12px;
	}
	.file-card {
		position: relative;
		border: 1px solid var(--line-soft);
		background: rgba(17, 20, 26, 0.65);
		border-radius: 12px;
		overflow: hidden;
		transition: border-color 0.15s, background 0.15s;
	}
	.file-card:hover {
		border-color: var(--line);
		background: var(--panel-2);
	}
	.file-card.selected {
		border-color: rgba(138, 180, 255, 0.55);
		box-shadow: 0 0 0 2px rgba(138, 180, 255, 0.12);
	}
	.card-main {
		display: flex;
		flex-direction: column;
		width: 100%;
		border: 0;
		background: transparent;
		padding: 12px;
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
		letter-spacing: 0.04em;
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
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mini-status {
		position: absolute;
		top: 10px;
		right: 10px;
		font-size: 10px;
		padding: 2px 6px;
		border-radius: 999px;
		background: var(--panel);
		border: 1px solid var(--line);
	}
	.card-actions {
		position: absolute;
		top: 7px;
		left: 7px;
		display: flex;
		gap: 3px;
		opacity: 0;
		transition: opacity 0.12s;
		background: rgba(8, 9, 12, 0.82);
		border: 1px solid var(--line-soft);
		border-radius: 8px;
		padding: 3px;
	}
	.file-card:hover .card-actions,
	.file-card.selected .card-actions {
		opacity: 1;
	}
	.mini-btn {
		width: 24px;
		height: 24px;
		border-radius: 6px;
		border: 1px solid transparent;
		background: transparent;
		color: var(--muted);
		cursor: pointer;
		font-size: 12px;
		line-height: 1;
	}
	.mini-btn:hover {
		background: var(--panel-2);
		color: var(--text);
	}
	.mini-btn.danger:hover {
		color: #ffc2cc;
		background: rgba(251, 113, 133, 0.12);
	}

	.row-list {
		border: 1px solid var(--line-soft);
		border-radius: 12px;
		overflow: hidden;
	}
	.row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 90px 170px 94px;
		align-items: center;
		gap: 10px;
		padding: 6px 10px;
		border-bottom: 1px solid var(--line-soft);
		font-size: 13px;
	}
	.row:last-child {
		border-bottom: 0;
	}
	.row:hover {
		background: var(--panel-2);
	}
	.row.selected {
		background: rgba(138, 180, 255, 0.08);
	}
	.row-head {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.07em;
		color: var(--faint);
		background: rgba(13, 16, 21, 0.6);
	}
	.row-main {
		display: flex;
		align-items: center;
		gap: 10px;
		border: 0;
		background: transparent;
		color: var(--text);
		cursor: pointer;
		text-align: left;
		min-width: 0;
		padding: 4px 0;
	}
	.row-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.row-size,
	.row-date {
		color: var(--faint);
		font-size: 12px;
	}
	.row-actions {
		display: flex;
		gap: 2px;
		justify-content: flex-end;
		opacity: 0;
	}
	.row:hover .row-actions,
	.row.selected .row-actions {
		opacity: 1;
	}

	.result-list {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.result-row {
		display: flex;
		align-items: flex-start;
		gap: 12px;
		width: 100%;
		text-align: left;
		border: 1px solid var(--line-soft);
		background: rgba(17, 20, 26, 0.55);
		border-radius: 10px;
		padding: 10px 12px;
		color: var(--text);
		cursor: pointer;
	}
	.result-row:hover {
		background: var(--panel-2);
		border-color: var(--line);
	}
	.result-main {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
		flex: 1;
	}
	.result-name {
		font-size: 13.5px;
		font-weight: 600;
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.result-path {
		font-family: var(--mono);
		font-size: 11px;
		color: var(--faint);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.result-snippet {
		font-size: 12px;
		color: var(--muted);
		line-height: 1.45;
		overflow: hidden;
		display: -webkit-box;
		line-clamp: 2;
		-webkit-line-clamp: 2;
		-webkit-box-orient: vertical;
	}
	.result-snippet :global(mark) {
		background: rgba(138, 180, 255, 0.22);
		color: #dce8ff;
		border-radius: 2px;
		padding: 0 1px;
	}
	.result-size,
	.result-date {
		font-size: 11.5px;
		color: var(--faint);
		white-space: nowrap;
		padding-top: 2px;
	}
	.share-row {
		display: flex;
		align-items: center;
		gap: 12px;
		border: 1px solid var(--line-soft);
		background: rgba(17, 20, 26, 0.55);
		border-radius: 10px;
		padding: 10px 12px;
	}
	.share-actions {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
		justify-content: flex-end;
	}
	.share-actions .btn {
		padding: 6px 9px;
		font-size: 12px;
	}
	.muted-note {
		color: var(--faint);
		font-size: 12px;
	}

	.pill {
		display: inline-flex;
		align-items: center;
		border-radius: 999px;
		padding: 1px 8px;
		font-size: 10.5px;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		border: 1px solid var(--line);
		color: var(--muted);
		background: var(--bg-2);
	}
	.pill.active,
	.pill.ok {
		color: #8ef5e3;
		border-color: rgba(94, 234, 212, 0.3);
		background: rgba(94, 234, 212, 0.08);
	}
	.pill.expired,
	.pill.warn {
		color: #ffd59a;
		border-color: rgba(251, 191, 36, 0.3);
		background: rgba(251, 191, 36, 0.08);
	}
	.pill.revoked,
	.pill.bad {
		color: #ffc2cc;
		border-color: rgba(251, 113, 133, 0.3);
		background: rgba(251, 113, 133, 0.08);
	}
	.pill.idle {
		color: var(--faint);
	}

	.upload-banner {
		margin: 10px 16px 0;
		padding: 10px 12px;
		border: 1px solid rgba(138, 180, 255, 0.25);
		background: rgba(138, 180, 255, 0.06);
		border-radius: 10px;
	}
	.upload-copy {
		display: flex;
		justify-content: space-between;
		font-size: 12px;
		color: var(--muted);
		margin-bottom: 7px;
	}
	.upload-track {
		height: 5px;
		border-radius: 999px;
		background: var(--bg-2);
		overflow: hidden;
	}
	.upload-track span {
		display: block;
		height: 100%;
		background: linear-gradient(90deg, var(--accent), var(--accent-2));
		transition: width 0.15s;
	}
	.banner.dismissable {
		margin: 10px 16px 0;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
	}
	.toast {
		position: absolute;
		bottom: 18px;
		left: 50%;
		transform: translateX(-50%);
		background: var(--panel-2);
		border: 1px solid var(--line);
		color: var(--text);
		border-radius: 999px;
		padding: 7px 16px;
		font-size: 12.5px;
		box-shadow: 0 12px 30px rgba(0, 0, 0, 0.45);
		z-index: 5;
	}

	.drop-overlay {
		position: absolute;
		inset: 0;
		background: rgba(8, 9, 12, 0.72);
		backdrop-filter: blur(2px);
		display: grid;
		place-items: center;
		z-index: 10;
		pointer-events: none;
	}
	.drop-card {
		border: 1px dashed rgba(138, 180, 255, 0.6);
		border-radius: 16px;
		padding: 28px 36px;
		background: rgba(17, 20, 26, 0.9);
		display: flex;
		flex-direction: column;
		gap: 5px;
		text-align: center;
	}
	.drop-card strong {
		font-size: 17px;
	}
	.drop-card span {
		color: var(--muted);
		font-size: 12.5px;
	}

	.empty-card {
		border: 1px dashed var(--line);
		border-radius: 14px;
		padding: 48px 24px;
		text-align: center;
		color: var(--muted);
		max-width: 520px;
		margin: 8vh auto 0;
	}
	.empty-card h2 {
		font-size: 18px;
		margin: 0 0 8px;
		color: var(--text);
	}
	.empty-card p {
		margin: 0;
		font-size: 13px;
	}
	.empty-actions {
		display: flex;
		gap: 8px;
		justify-content: center;
		margin-top: 18px;
	}

	.details {
		border-left: 1px solid var(--line-soft);
		background: rgba(13, 16, 21, 0.55);
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.details-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 14px;
		border-bottom: 1px solid var(--line-soft);
	}
	.details-title {
		flex: 1;
		font-size: 13.5px;
		font-weight: 650;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.details-body {
		overflow: auto;
		padding: 14px;
		min-height: 0;
	}
	.preview-box {
		border: 1px solid var(--line-soft);
		border-radius: 10px;
		overflow: hidden;
		background: var(--bg-2);
		margin-bottom: 12px;
	}
	.preview-image {
		display: block;
		width: 100%;
		height: 260px;
		object-fit: contain;
		background: #05060a;
	}
	.preview-frame {
		width: 100%;
		height: 300px;
		border: 0;
		background: #fff;
	}
	.preview-video {
		width: 100%;
		max-height: 300px;
		display: block;
		background: #05060a;
	}
	.preview-audio {
		width: 100%;
		padding: 14px;
	}
	.preview-text {
		margin: 0;
		padding: 12px;
		max-height: 320px;
		overflow: auto;
		font-family: var(--mono);
		font-size: 11.5px;
		line-height: 1.55;
		white-space: pre-wrap;
		word-break: break-word;
		color: #c8d2e6;
	}
	.preview-none {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 9px;
		padding: 30px 14px;
		color: var(--faint);
		font-size: 12px;
	}
	.detail-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-bottom: 4px;
	}
	.detail-actions .btn {
		padding: 7px 10px;
		font-size: 12px;
	}
	.detail-section {
		margin-top: 18px;
	}
	.detail-section h3 {
		display: flex;
		align-items: center;
		gap: 7px;
		font-size: 11px;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--faint);
		margin: 0 0 8px;
		font-weight: 600;
	}
	.kv {
		display: grid;
		grid-template-columns: 92px minmax(0, 1fr);
		gap: 5px 10px;
		margin: 0;
		font-size: 12px;
	}
	.kv dt {
		color: var(--faint);
	}
	.kv dd {
		margin: 0;
		color: var(--text);
		overflow-wrap: anywhere;
	}
	.kv dd.mono,
	.mono {
		font-family: var(--mono);
		font-size: 11px;
	}
	.kv dd.hash {
		color: var(--muted);
	}
	.metadata {
		max-height: 260px;
		overflow: auto;
	}
	.metadata dt {
		max-width: 92px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.detail-error {
		margin: 8px 0 0;
		padding: 7px 9px;
		border: 1px solid rgba(251, 113, 133, 0.25);
		background: rgba(251, 113, 133, 0.06);
		border-radius: 7px;
		color: #ffc2cc;
		font-size: 11.5px;
		overflow-wrap: anywhere;
	}
	.detail-note {
		color: var(--faint);
		font-size: 11.5px;
		margin: 8px 0 0;
		line-height: 1.5;
	}
	.vision-text {
		font-size: 12.5px;
		color: #cfd8ea;
		line-height: 1.55;
		margin: 0 0 8px;
	}
	.vision-ocr {
		margin-bottom: 8px;
	}
	.vision-ocr pre {
		margin: 4px 0 0;
		padding: 8px;
		background: var(--bg-2);
		border: 1px solid var(--line-soft);
		border-radius: 7px;
		font-family: var(--mono);
		font-size: 11px;
		white-space: pre-wrap;
		max-height: 160px;
		overflow: auto;
	}
	.label {
		font-size: 10.5px;
		letter-spacing: 0.07em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: 4px;
		margin: 7px 0;
	}
	.tag {
		font-size: 10.5px;
		padding: 2px 7px;
		border-radius: 999px;
		background: rgba(192, 132, 252, 0.09);
		border: 1px solid rgba(192, 132, 252, 0.22);
		color: #ddc9ff;
	}
	.extracted summary {
		cursor: pointer;
		font-size: 12px;
		color: var(--muted);
	}
	.extracted pre {
		margin: 8px 0 0;
		padding: 9px;
		background: var(--bg-2);
		border: 1px solid var(--line-soft);
		border-radius: 7px;
		font-family: var(--mono);
		font-size: 11px;
		white-space: pre-wrap;
		max-height: 240px;
		overflow: auto;
	}
	.node-shares {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin-bottom: 9px;
	}
	.node-share {
		display: flex;
		align-items: center;
		gap: 7px;
		flex-wrap: wrap;
		font-size: 11px;
		color: var(--muted);
		border: 1px solid var(--line-soft);
		border-radius: 7px;
		padding: 6px 8px;
	}
	.node-share-expiry {
		flex: 1;
		color: var(--faint);
	}
	.link-btn {
		border: 0;
		background: transparent;
		color: var(--accent);
		font-size: 11px;
		cursor: pointer;
		padding: 2px 3px;
	}
	.link-btn:hover {
		text-decoration: underline;
	}
	.link-btn.danger {
		color: #ff9fb0;
	}

	.modal-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(4, 5, 8, 0.72);
		backdrop-filter: blur(3px);
		display: grid;
		place-items: center;
		padding: 20px;
		z-index: 50;
	}
	.modal {
		width: 100%;
		max-width: 560px;
		max-height: 86vh;
		overflow: auto;
		background: linear-gradient(180deg, var(--panel-2), var(--panel));
		border: 1px solid var(--line);
		border-radius: 16px;
		padding: 18px;
		box-shadow: 0 30px 90px rgba(0, 0, 0, 0.6);
	}
	.modal-head {
		display: flex;
		justify-content: space-between;
		gap: 12px;
		align-items: flex-start;
	}
	.modal-head h2 {
		font-size: 17px;
		margin: 0;
		letter-spacing: -0.01em;
	}
	.modal-head .detail-note {
		max-width: 440px;
	}
	.share-create {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
		margin-top: 18px;
	}
	.share-create .label {
		width: 100%;
	}
	.share-create select {
		flex: 1;
		min-width: 160px;
		padding: 9px 10px;
	}
	.created-link {
		margin-top: 16px;
		padding-top: 14px;
		border-top: 1px solid var(--line-soft);
	}
	.link-row {
		display: flex;
		gap: 7px;
		margin-top: 6px;
	}
	.link-row .input {
		flex: 1;
		font-size: 11.5px;
		padding: 8px 10px;
	}
	.share-existing {
		margin-top: 16px;
		padding-top: 14px;
		border-top: 1px solid var(--line-soft);
	}
	.share-existing-row {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
		padding: 7px 0;
		border-bottom: 1px solid var(--line-soft);
		font-size: 11.5px;
	}
	.share-existing-row:last-child {
		border-bottom: 0;
	}
	.share-existing-meta {
		flex: 1;
		color: var(--faint);
	}
	.modal-foot {
		display: flex;
		justify-content: flex-end;
		margin-top: 18px;
	}

	@media (max-width: 1280px) {
		.drive-shell.with-details {
			grid-template-columns: 250px minmax(0, 1fr) 330px;
		}
	}
	@media (max-width: 1100px) {
		.drive-shell,
		.drive-shell.with-details {
			grid-template-columns: 250px minmax(0, 1fr);
		}
		.details {
			position: absolute;
			top: 56px;
			right: 0;
			bottom: 0;
			width: min(380px, 92vw);
			background: var(--panel);
			box-shadow: -20px 0 50px rgba(0, 0, 0, 0.45);
			z-index: 20;
		}
	}
	@media (max-width: 760px) {
		.drive-shell,
		.drive-shell.with-details {
			grid-template-columns: minmax(0, 1fr);
		}
		.details {
			width: 100vw;
		}
		.card-grid {
			grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
		}
		.row {
			grid-template-columns: minmax(0, 1fr) 72px;
		}
		.row-date,
		.row-head span:nth-child(3) {
			display: none;
		}
	}
</style>
