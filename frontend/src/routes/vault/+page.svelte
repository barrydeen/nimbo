<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { goto, replaceState } from '$app/navigation';
	import {
		api,
		errorMessage,
		isUnauthorized,
		type DocMeta,
		type SearchResult,
		type User
	} from '$lib/api';
	import Editor from '$lib/components/Editor.svelte';
	import AgentPanel from '$lib/components/AgentPanel.svelte';
	import { applyDraftEditsToText, type AgentDraft, type AgentDraftEdit } from '$lib/agent';
	import { RequestScope, isAbortError } from '$lib/generations';
	import { normalizeWikiTarget, renderMarkdown } from '$lib/markdown';
	import { sanitizeSnippet } from '$lib/sanitize';

	let user: User | null = null;
	let instanceName = 'Nimbo';

	let docs: DocMeta[] = [];
	let selected: string | null = null;
	let content = '';
	let hash = '';
	let dirty = false;
	let saving = false;
	let saveQueued = false;
	let autosaveTimer: ReturnType<typeof setTimeout> | null = null;
	let split = true;
	let aiEnabled = false;
	let editorKey = 0;
	let contentVersion = 0;
	let assistantOpen = false;

	let backlinks: { path: string; title: string }[] = [];
	let outlinks: { target: string; path: string | null; exists: boolean }[] = [];

	let query = '';
	let results: SearchResult[] = [];
	let searching = false;
	let searchMode = false;

	let showNew = false;
	let newPath = '';

	let errorMsg = '';
	let statusMsg = '';
	let statusKey = 0;
	let statusTimer: ReturnType<typeof setTimeout> | null = null;
	let initError = '';
	let initBusy = false;
	let disposed = false;
	let loggingOut = false;

	let previewHtml = '';
	let previewTimer: ReturnType<typeof setTimeout> | null = null;

	let searchTimer: ReturnType<typeof setTimeout> | null = null;
	let editorRef: Editor;
	let selection: { text: string; from: number; to: number } | null = null;

	const docScope = new RequestScope();
	const linkScope = new RequestScope();
	const searchScope = new RequestScope();
	const AUTOSAVE_DELAY = 3000;

	onMount(() => {
		void init();
		return () => {
			disposed = true;
			docScope.invalidate();
			linkScope.invalidate();
			searchScope.invalidate();
			if (previewTimer) clearTimeout(previewTimer);
			if (searchTimer) clearTimeout(searchTimer);
			if (statusTimer) clearTimeout(statusTimer);
			if (autosaveTimer) clearTimeout(autosaveTimer);
		};
	});

	onDestroy(() => {
		disposed = true;
	});

	function message(err: unknown): string {
		return errorMessage(err);
	}

	function flash(text: string) {
		statusMsg = text;
		statusKey += 1;
		if (statusTimer) clearTimeout(statusTimer);
		statusTimer = setTimeout(() => (statusMsg = ''), 1600);
	}

	function clearAutosave() {
		if (autosaveTimer) {
			clearTimeout(autosaveTimer);
			autosaveTimer = null;
		}
	}

	function scheduleAutosave() {
		if (autosaveTimer) return;
		autosaveTimer = setTimeout(() => {
			autosaveTimer = null;
			if (dirty) void save();
		}, AUTOSAVE_DELAY);
	}

	async function init() {
		initBusy = true;
		initError = '';
		errorMsg = '';
		try {
			let boot;
			try {
				boot = await api.bootstrap();
			} catch (err) {
				if (disposed) return;
				if (isUnauthorized(err)) {
					await goto('/login');
				} else {
					initError = `Could not reach the server: ${message(err)}`;
				}
				return;
			}
			if (disposed) return;
			if (!boot.setup_complete) {
				await goto('/setup');
				return;
			}
			if (!boot.user) {
				await goto('/login');
				return;
			}
			user = boot.user;
			instanceName = boot.instance_name;

			try {
				await refresh();
				if (disposed) return;
				const path = new URLSearchParams(location.search).get('path');
				if (path) {
					await openDoc(path);
				}
			} catch (err) {
				if (disposed) return;
				if (isUnauthorized(err)) {
					await goto('/login');
					return;
				}
				initError = `Your documents could not be loaded: ${message(err)}`;
			}
		} finally {
			if (!disposed) initBusy = false;
		}
	}

	function warn(event: BeforeUnloadEvent) {
		if (dirty) {
			event.preventDefault();
			event.returnValue = '';
		}
	}

	async function refresh() {
		const tree = await api.tree();
		docs = tree.documents;
	}

	function schedulePreview(immediate = false) {
		if (previewTimer) {
			clearTimeout(previewTimer);
			previewTimer = null;
		}
		if (!split) return;
		if (immediate) {
			previewHtml = renderMarkdown(content);
			return;
		}
		previewTimer = setTimeout(() => {
			previewTimer = null;
			previewHtml = renderMarkdown(content);
		}, 180);
	}

	function dedupeLinks(
		links: { target: string; path: string | null; exists: boolean }[]
	): { target: string; path: string | null; exists: boolean }[] {
		const seen = new Set<string>();
		return links.filter((link) => {
			const key = link.target.toLowerCase();
			if (seen.has(key)) return false;
			seen.add(key);
			return true;
		});
	}

	function dedupeBacklinks(
		links: { path: string; title: string }[]
	): { path: string; title: string }[] {
		const seen = new Set<string>();
		return links.filter((link) => {
			const key = link.path.toLowerCase();
			if (seen.has(key)) return false;
			seen.add(key);
			return true;
		});
	}

	async function loadLinks(path: string, docOp: number) {
		const op = linkScope.begin();
		try {
			const connections = await api.backlinks(path, op.signal);
			if (!docScope.isCurrent(docOp) || selected !== path || !linkScope.isCurrent(op.id)) return;
			backlinks = dedupeBacklinks(connections.backlinks);
			outlinks = dedupeLinks(connections.links);
		} catch (err) {
			if (isAbortError(err) || !linkScope.isCurrent(op.id)) return;
			if (isUnauthorized(err)) {
				void goto('/login');
				return;
			}
			// A link-index failure must not replace the editor contents.
			errorMsg = `Links could not be loaded: ${message(err)}`;
		}
	}

	async function openDoc(path: string, options: { force?: boolean } = {}) {
		if (dirty && !options.force && !confirm('Discard unsaved changes?')) return;
		clearAutosave();
		const op = docScope.begin();
		const versionAtStart = contentVersion;
		errorMsg = '';
		try {
			const doc = await api.getDoc(path, op.signal);
			if (!docScope.isCurrent(op.id)) return;
			if (dirty && contentVersion !== versionAtStart) {
				errorMsg =
					'The document changed while it was loading, so it was not replaced. Save or discard your edits first.';
				return;
			}
			clearAutosave();
			selected = path;
			selection = null;
			content = doc.content;
			hash = doc.hash;
			dirty = false;
			contentVersion += 1;
			editorKey += 1;
			errorMsg = '';
			searchMode = false;
			backlinks = [];
			outlinks = [];
			saveQueued = false;
			replaceState(`?path=${encodeURIComponent(path)}`, {});
			schedulePreview(true);
			await loadLinks(path, op.id);
		} catch (err) {
			if (isAbortError(err) || !docScope.isCurrent(op.id)) return;
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			errorMsg = message(err);
		}
	}

	async function save(): Promise<boolean> {
		clearAutosave();
		if (!selected || !dirty) return true;
		if (saving) {
			saveQueued = true;
			return false;
		}
		const path = selected;
		const submitted = content;
		const baseHash = hash;
		const version = contentVersion;
		saving = true;
		errorMsg = '';
		let ok = true;
		try {
			const res = await api.saveDoc({ path, content: submitted, base_hash: baseHash });
			if (selected === path) {
				hash = res.hash;
				// Only clear dirty when the editor still holds exactly what was sent.
				dirty = content !== submitted || contentVersion !== version;
				if (!dirty) flash('Saved');
			}
			try {
				await refresh();
			} catch (err) {
				if (isUnauthorized(err)) {
					await goto('/login');
					ok = false;
				}
			}
			if (selected === path) await loadLinks(path, docScope.current);
		} catch (err) {
			ok = false;
			if (isUnauthorized(err)) {
				await goto('/login');
			} else if (selected === path) {
				errorMsg = message(err);
			}
		} finally {
			saving = false;
			if (saveQueued) {
				saveQueued = false;
				if (selected === path && dirty) void save();
			}
		}
		return ok;
	}

	async function createDoc() {
		const raw = newPath.trim();
		if (!raw) return;
		if (dirty && !confirm('Discard unsaved changes?')) return;
		clearAutosave();
		const path = raw.endsWith('.md') ? raw : `${raw}.md`;
		const title = path.split('/').pop()?.replace(/\.md$/, '') ?? 'Untitled';
		try {
			await api.saveDoc({ path, content: `# ${title}\n\n`, base_hash: null });
			newPath = '';
			showNew = false;
			await refresh();
			dirty = false;
			await openDoc(path, { force: true });
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			errorMsg = message(err);
		}
	}

	async function renameDoc() {
		if (!selected || saving) return;
		const currentPath = selected;
		const next = prompt('New path', currentPath);
		if (!next || next === currentPath) return;

		// Never silently drop edits: persist the current in-memory content first.
		if (dirty) {
			if (!confirm('Save your changes before renaming?')) return;
			await save();
			if (dirty) {
				errorMsg =
					'Rename cancelled because the save did not complete. Your unsaved changes are still here.';
				return;
			}
		}

		try {
			const res = await api.renameDoc(currentPath, next);
			if (selected !== currentPath) return;
			await refresh();
			await openDoc(res.path, { force: true });
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			errorMsg = message(err);
		}
	}

	async function deleteDoc() {
		if (!selected) return;
		const path = selected;
		const warning = dirty ? ' Unsaved edits will be discarded.' : '';
		if (!confirm(`Delete ${path}? This removes the file from disk.${warning}`)) return;
		clearAutosave();

		// Invalidate any older open/save continuation for this document.
		const op = docScope.begin();
		linkScope.invalidate();
		try {
			await api.deleteDoc(path);
			if (selected === path) {
				docScope.invalidate();
				selected = null;
				content = '';
				hash = '';
				dirty = false;
				saveQueued = false;
				contentVersion += 1;
				backlinks = [];
				outlinks = [];
				previewHtml = '';
				replaceState('/vault', {});
			}
			await refresh();
		} catch (err) {
			if (!docScope.isCurrent(op.id)) return;
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			errorMsg = message(err);
		}
	}

	async function runSearch() {
		const q = query.trim();
		errorMsg = '';
		if (!q) {
			clearSearch();
			return;
		}
		const op = searchScope.begin();
		searching = true;
		searchMode = true;
		try {
			const res = await api.search(q, op.signal);
			if (!searchScope.isCurrent(op.id)) return;
			results = res.results;
		} catch (err) {
			if (isAbortError(err) || !searchScope.isCurrent(op.id)) return;
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			errorMsg = message(err);
		} finally {
			if (searchScope.isCurrent(op.id)) searching = false;
		}
	}

	function clearSearch() {
		query = '';
		results = [];
		searchMode = false;
		searching = false;
		searchScope.invalidate();
	}

	function resolveWikiTarget(target: string): string | null {
		const wanted = normalizeWikiTarget(target);
		if (!wanted) return null;
		const lower = wanted.toLowerCase();
		const exact = docs.find((d) => d.path.toLowerCase() === lower);
		if (exact) return exact.path;
		const byName = docs.find((d) => d.path.toLowerCase().endsWith(`/${lower}`));
		return byName ? byName.path : null;
	}

	function openWiki(target: string) {
		const path = resolveWikiTarget(target);
		if (path) {
			void openDoc(path);
		} else {
			errorMsg = `No document named "${target}"`;
		}
	}

	function openFromAssistant(path: string) {
		if (typeof window !== 'undefined' && window.innerWidth <= 1100) {
			assistantOpen = false;
		}
		void openDoc(path);
	}

	function onPreviewClick(event: MouseEvent) {
		const anchor = (event.target as HTMLElement).closest('a[data-wiki]') as HTMLAnchorElement | null;
		if (!anchor) return;
		event.preventDefault();
		openWiki(decodeURIComponent(anchor.dataset.wiki || ''));
	}

	function snippetHtml(snippet: string): string {
		return sanitizeSnippet(snippet);
	}

	async function logout() {
		if (loggingOut) return;
		loggingOut = true;
		errorMsg = '';
		try {
			await api.logout();
			await goto('/login');
		} catch (err) {
			errorMsg = `Sign out failed: ${message(err)}. You are still signed in on this device; retry when the server is reachable.`;
		} finally {
			loggingOut = false;
		}
	}

	async function applyDraft(
		draft: AgentDraft,
		edits: AgentDraftEdit[]
	): Promise<{ ok: boolean; error?: string; warning?: string }> {
		if (draft.kind === 'create') {
			if (docs.some((doc) => doc.path === draft.path)) {
				return { ok: false, error: `A document already exists at ${draft.path}.` };
			}
			if (dirty) {
				const saved = await save();
				if (!saved || dirty) {
					return {
						ok: false,
						error: 'Save the current note before creating a new document.'
					};
				}
			}
			try {
				await api.saveDoc({
					path: draft.path,
					content: draft.content ?? '',
					base_hash: null
				});
				await refresh();
				flash('Created');
				return { ok: true };
			} catch (err) {
				if (isUnauthorized(err)) {
					await goto('/login');
					return { ok: false, error: 'Your session expired.' };
				}
				return { ok: false, error: message(err) };
			}
		}

		if (draft.path === selected) {
			if (!editorRef) return { ok: false, error: 'The editor is not ready.' };
			let result: { ok: boolean; error?: string; ranges?: { from: number; to: number }[] };
			if (draft.kind === 'edit') {
				result = editorRef.applyDraftEdits(edits);
			} else if (draft.kind === 'append') {
				result = editorRef.appendContent(draft.content ?? '');
			} else if (draft.kind === 'replace') {
				result = editorRef.replaceContent(draft.content ?? '');
			} else {
				return { ok: false, error: 'Unknown draft kind.' };
			}
			if (!result.ok) return { ok: false, error: result.error };
			await tick();
			const saved = await save();
			if (!saved) {
				return {
					ok: true,
					warning:
						'Applied in the editor, but saving failed. Your changes are still in the buffer.'
				};
			}
			if (dirty) {
				return {
					ok: true,
					warning: 'Applied and saved, but newer edits are still unsaved.'
				};
			}
			if (result.ranges?.[0]) {
				editorRef.revealRange(result.ranges[0].from, result.ranges[0].to);
			}
			flash('Applied');
			return { ok: true };
		}

		try {
			const doc = await api.getDoc(draft.path);
			let next = doc.content;
			if (draft.kind === 'edit') {
				const applied = applyDraftEditsToText(doc.content, edits);
				if (!applied.ok) return { ok: false, error: applied.error };
				next = applied.content;
			} else if (draft.kind === 'append') {
				next = doc.content + (draft.content ?? '');
			} else if (draft.kind === 'replace') {
				next = draft.content ?? '';
			} else {
				return { ok: false, error: 'Unknown draft kind.' };
			}
			await api.saveDoc({ path: draft.path, content: next, base_hash: doc.hash });
			await refresh();
			flash('Saved');
			return { ok: true };
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return { ok: false, error: 'Your session expired.' };
			}
			return { ok: false, error: message(err) };
		}
	}

	async function undoDraft(draft: AgentDraft): Promise<{ ok: boolean; error?: string }> {
		if (draft.path !== selected || !editorRef) {
			return { ok: false, error: 'Open the document to undo this change.' };
		}
		if (!editorRef.undo()) {
			return { ok: false, error: 'There is nothing to undo in the editor.' };
		}
		await tick();
		const saved = await save();
		if (!saved || dirty) {
			return { ok: false, error: 'Undone in the editor, but saving failed.' };
		}
		return { ok: true };
	}

	async function toggleSplit() {
		split = !split;
		if (split) {
			await tick();
			schedulePreview(true);
		}
	}
</script>

<svelte:window on:beforeunload={warn} />

<div class="shell">
	<header class="topbar">
		<div class="brand">
			<span class="mark">N</span>
			<span class="name">{instanceName}</span>
		</div>
		<div class="instance">Markdown vault</div>
		<div class="spacer"></div>
		{#if user}
			<div class="user-chip">
				<span class="avatar">{user.username.slice(0, 1).toUpperCase()}</span>
				<span>{user.username}</span>
			</div>
		{/if}
		<button
			class="btn ghost assistant-toggle"
			on:click={() => (assistantOpen = true)}
			title="Open the assistant"
		>
			Assistant
		</button>
		<button class="btn ghost" on:click={() => goto('/drive')} disabled={loggingOut}>Files</button>
		{#if user?.role === 'admin'}
			<button class="btn ghost" on:click={() => goto('/settings')} disabled={loggingOut}>Settings</button>
		{/if}
		<button class="btn ghost" on:click={logout} disabled={loggingOut}>
			{loggingOut ? 'Signing out…' : 'Sign out'}
		</button>
	</header>

	{#if initError}
		<div class="init-error">
			<div class="banner">{initError}</div>
			<button class="btn primary" on:click={init} disabled={initBusy}>
				{initBusy ? 'Retrying…' : 'Retry'}
			</button>
		</div>
	{/if}

	<aside class="sidebar">
		<div class="sidebar-head">
			<div class="searchbox">
				<input
					placeholder="Search everything…"
					bind:value={query}
					on:input={() => {
						if (searchTimer) clearTimeout(searchTimer);
						searchTimer = setTimeout(() => {
							if (query.trim()) runSearch();
							else clearSearch();
						}, 250);
					}}
					on:keydown={(e) => {
						if (e.key === 'Enter') runSearch();
						if (e.key === 'Escape') clearSearch();
					}}
				/>
			</div>
		</div>

		{#if searchMode}
			<div class="sidebar-section">
				<span>{searching ? 'Searching…' : `${results.length} results`}</span>
				<button class="btn ghost" on:click={clearSearch}>Clear</button>
			</div>
			<div class="doclist">
				{#each results as result (result.path)}
					<button class="doc-item result-item" on:click={() => openDoc(result.path)}>
						<span class="t">{result.title || result.path}</span>
						<span class="p">{result.path}</span>
						{#if result.snippet}
							<div class="snip">{@html snippetHtml(result.snippet)}</div>
						{/if}
					</button>
				{:else}
					<div class="rail"><span class="empty">No matches.</span></div>
				{/each}
			</div>
		{:else}
			<div class="sidebar-section">
				<span>Documents</span>
				<button class="btn ghost" on:click={() => (showNew = !showNew)}>+ New</button>
			</div>
			{#if showNew}
				<div class="newdoc">
					<input
						placeholder="notes/idea.md"
						bind:value={newPath}
						on:keydown={(e) => {
							if (e.key === 'Enter') createDoc();
							if (e.key === 'Escape') showNew = false;
						}}
					/>
					<button class="btn" on:click={createDoc}>Add</button>
				</div>
			{/if}
			<div class="doclist">
				{#each docs as doc (doc.path)}
					<button
						class="doc-item"
						class:active={selected === doc.path}
						on:click={() => openDoc(doc.path)}
					>
						<span class="t">{doc.title || doc.path}</span>
						<span class="p">{doc.path}</span>
					</button>
				{:else}
					<div class="rail"><span class="empty">No documents yet.</span></div>
				{/each}
			</div>
		{/if}
	</aside>

	<main class="main">
		{#if errorMsg}
			<div class="banner dismissable" style="margin: 10px 14px 0;">
				<span>{errorMsg}</span>
				<button class="btn ghost" on:click={() => (errorMsg = '')}>Dismiss</button>
			</div>
		{/if}

		{#if selected}
			<div class="toolbar">
				<span class="path">{selected}</span>
				{#if dirty}<span class="dot dirty" title="Unsaved changes"></span>{/if}
				{#if statusMsg}
					{#key statusKey}
						<span class="save-flash" aria-live="polite">
							<span class="save-check">✓</span>{statusMsg}
						</span>
					{/key}
				{/if}
				<div class="spacer"></div>
				<button class="btn" class:active={split} on:click={toggleSplit}>
					{split ? 'Single pane' : 'Split preview'}
				</button>
				<button class="btn ghost" on:click={renameDoc} disabled={saving}>Rename</button>
				<button class="btn danger" on:click={deleteDoc}>Delete</button>
			</div>

			<div class="workspace" class:split>
				<div class="editor-pane">
					{#key `${selected}:${editorKey}`}
						<Editor
							bind:this={editorRef}
							bind:aiEnabled
							value={content}
							docPaths={docs.map((d) => d.path)}
							onchange={(next) => {
								content = next;
								dirty = true;
								contentVersion += 1;
								schedulePreview();
								scheduleAutosave();
							}}
							onsave={save}
							onunauthorized={() => goto('/login')}
							onselection={(next) => (selection = next)}
						/>
					{/key}
				</div>
				{#if split}
					<div class="preview-pane">
						<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
						<div class="preview" on:click={onPreviewClick}>
							<div class="markdown-body">{@html previewHtml}</div>
						</div>
					</div>
				{/if}
			</div>
		{:else}
			<div class="empty-state">
				<div class="inner">
					<div class="brand" style="justify-content: center;">
						<span class="mark">N</span>
						<span class="name">{instanceName}</span>
					</div>
					<h2>Your vault is empty</h2>
					<p>
						Documents are plain <code>.md</code> files on disk. Create one, then link it with
						<code>[[wikilinks]]</code> — backlinks and search update automatically.
					</p>
					<button class="btn primary" on:click={() => (showNew = true)}>
						Create your first document
					</button>
					<p style="margin-top: 18px;">
						Changes save automatically. <span class="kbd">Ctrl</span> +
						<span class="kbd">S</span> saves immediately.
					</p>
				</div>
			</div>
		{/if}
	</main>

	{#if assistantOpen}
		<button
			class="agent-backdrop"
			aria-label="Close assistant"
			on:click={() => (assistantOpen = false)}
		></button>
	{/if}
	<AgentPanel
		path={selected}
		docContent={content}
		docHash={hash}
		docVersion={contentVersion}
		{dirty}
		{selection}
		{docs}
		{user}
		{backlinks}
		{outlinks}
		open={assistantOpen}
		onclose={() => (assistantOpen = false)}
		onapplydraft={applyDraft}
		onundo={undoDraft}
		onopendoc={openFromAssistant}
		onnotify={flash}
		onconfigure={() => goto('/settings')}
	/>
</div>
