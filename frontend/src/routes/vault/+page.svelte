<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { beforeNavigate, goto, replaceState } from '$app/navigation';
	import {
		api,
		errorMessage,
		isUnauthorized,
		type DocMeta,
		type SearchResult,
		type User
	} from '$lib/api';
	import Editor from '$lib/components/Editor.svelte';
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
	let split = true;
	let aiEnabled = false;
	let editorKey = 0;
	let contentVersion = 0;

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
	let statusTimer: ReturnType<typeof setTimeout> | null = null;
	let initError = '';
	let initBusy = false;
	let disposed = false;
	let loggingOut = false;

	let previewHtml = '';
	let previewTimer: ReturnType<typeof setTimeout> | null = null;

	let railTab: 'links' | 'chat' = 'links';
	let agentMessages: {
		id: number;
		role: 'user' | 'assistant';
		content: string;
	}[] = [];
	let agentInput = '';
	let agentBusy = false;
	let agentError = '';
	let msgSeq = 0;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;

	const docScope = new RequestScope();
	const linkScope = new RequestScope();
	const searchScope = new RequestScope();
	const MAX_AGENT_MESSAGES = 40;

	let agentAbort: AbortController | null = null;
	let agentRun = 0;
	let agentFlushHandle: number | null = null;
	let agentPending = '';
	let agentAssistantId = 0;

	onMount(() => {
		void init();
		return () => {
			disposed = true;
			abortAgent();
			docScope.invalidate();
			linkScope.invalidate();
			searchScope.invalidate();
			if (previewTimer) clearTimeout(previewTimer);
			if (searchTimer) clearTimeout(searchTimer);
			if (statusTimer) clearTimeout(statusTimer);
		};
	});

	onDestroy(() => {
		disposed = true;
		abortAgent();
	});

	beforeNavigate(() => {
		abortAgent();
	});

	function message(err: unknown): string {
		return errorMessage(err);
	}

	function flash(text: string) {
		statusMsg = text;
		if (statusTimer) clearTimeout(statusTimer);
		statusTimer = setTimeout(() => (statusMsg = ''), 1600);
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
			selected = path;
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

	async function save() {
		if (!selected || !dirty) return;
		if (saving) {
			saveQueued = true;
			return;
		}
		const path = selected;
		const submitted = content;
		const baseHash = hash;
		const version = contentVersion;
		saving = true;
		errorMsg = '';
		try {
			const res = await api.saveDoc({ path, content: submitted, base_hash: baseHash });
			if (selected === path) {
				hash = res.hash;
				// Only clear dirty when the editor still holds exactly what was sent.
				dirty = content !== submitted || contentVersion !== version;
			}
			flash('Saved');
			try {
				await refresh();
			} catch (err) {
				if (isUnauthorized(err)) {
					await goto('/login');
					return;
				}
			}
			if (selected === path) await loadLinks(path, docScope.current);
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			if (selected === path) errorMsg = message(err);
		} finally {
			saving = false;
			if (saveQueued) {
				saveQueued = false;
				if (selected === path && dirty) void save();
			}
		}
	}

	async function createDoc() {
		const raw = newPath.trim();
		if (!raw) return;
		if (dirty && !confirm('Discard unsaved changes?')) return;
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
		abortAgent();
		try {
			await api.logout();
			await goto('/login');
		} catch (err) {
			errorMsg = `Sign out failed: ${message(err)}. You are still signed in on this device; retry when the server is reachable.`;
		} finally {
			loggingOut = false;
		}
	}

	function trimAgentHistory(messages: typeof agentMessages): typeof agentMessages {
		if (messages.length <= MAX_AGENT_MESSAGES) return messages;
		return messages.slice(messages.length - MAX_AGENT_MESSAGES);
	}

	function flushAgent() {
		if (agentFlushHandle !== null) {
			cancelAnimationFrame(agentFlushHandle);
			agentFlushHandle = null;
		}
		if (!agentPending) return;
		const append = agentPending;
		agentPending = '';
		agentMessages = trimAgentHistory(
			agentMessages.map((entry) =>
				entry.id === agentAssistantId
					? { ...entry, content: entry.content + append }
					: entry
			)
		);
	}

	function scheduleAgentFlush() {
		if (agentFlushHandle !== null) return;
		agentFlushHandle = requestAnimationFrame(() => {
			agentFlushHandle = null;
			flushAgent();
		});
	}

	function abortAgent() {
		agentRun += 1;
		agentAbort?.abort();
		agentAbort = null;
		flushAgent();
		agentPending = '';
		if (agentFlushHandle !== null) {
			cancelAnimationFrame(agentFlushHandle);
			agentFlushHandle = null;
		}
		agentBusy = false;
	}

	function stopAgent() {
		if (!agentBusy) return;
		abortAgent();
		agentError = 'Stopped.';
	}

	async function sendAgent() {
		const userMessage = agentInput.trim();
		if (!userMessage || agentBusy) return;
		agentInput = '';
		agentError = '';
		const run = ++agentRun;
		const abort = new AbortController();
		agentAbort = abort;
		agentBusy = true;
		const assistantId = ++msgSeq;
		agentAssistantId = assistantId;
		agentMessages = trimAgentHistory([
			...agentMessages,
			{ id: ++msgSeq, role: 'user', content: userMessage },
			{ id: assistantId, role: 'assistant', content: '' }
		]);

		try {
			const res = await fetch('/api/agent/chat', {
				method: 'POST',
				credentials: 'include',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ message: userMessage, path: selected }),
				signal: abort.signal
			});
			if (res.status === 401) {
				await goto('/login');
				return;
			}
			if (!res.ok) {
				const data = await res.json().catch(() => ({}));
				throw new Error(data.error || `${res.status} ${res.statusText}`);
			}
			const reader = res.body?.getReader();
			if (!reader) throw new Error('streaming is not supported in this browser');

			const decoder = new TextDecoder();
			let buffer = '';
			while (true) {
				const { done, value } = await reader.read();
				if (done) break;
				if (run !== agentRun || abort.signal.aborted) {
					await reader.cancel().catch(() => undefined);
					break;
				}
				buffer += decoder.decode(value, { stream: true });
				buffer = buffer.replace(/\r\n/g, '\n');
				let cut: number;
				while ((cut = buffer.indexOf('\n\n')) !== -1) {
					const frame = buffer.slice(0, cut);
					buffer = buffer.slice(cut + 2);
					let event = 'message';
					const dataLines: string[] = [];
					for (const line of frame.split('\n')) {
						if (line.startsWith('event:')) event = line.slice(6).trim();
						else if (line.startsWith('data:')) dataLines.push(line.slice(5).replace(/^ /, ''));
					}
					const data = dataLines.join('\n');
					if (event === 'error') {
						agentError = data;
					} else if (event === 'done') {
						flushAgent();
					} else if (data) {
						agentPending += data;
						scheduleAgentFlush();
					}
				}
			}
			flushAgent();
		} catch (err) {
			if (!abort.signal.aborted && run === agentRun) agentError = message(err);
		} finally {
			if (run === agentRun) {
				flushAgent();
				agentBusy = false;
				if (agentAbort === abort) agentAbort = null;
			}
		}
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
				{#if statusMsg}<span class="path" style="color: var(--good)">{statusMsg}</span>{/if}
				<div class="spacer"></div>
				<button class="btn" class:active={split} on:click={toggleSplit}>
					{split ? 'Single pane' : 'Split preview'}
				</button>
				<button class="btn primary" on:click={save} disabled={!dirty || saving}>
					{saving ? 'Saving…' : 'Save'}
				</button>
				<button class="btn ghost" on:click={renameDoc} disabled={saving}>Rename</button>
				<button class="btn danger" on:click={deleteDoc}>Delete</button>
			</div>

			<div class="workspace" class:split>
				<div class="editor-pane">
					{#key `${selected}:${editorKey}`}
						<Editor
							bind:aiEnabled
							value={content}
							docPaths={docs.map((d) => d.path)}
							onchange={(next) => {
								content = next;
								dirty = true;
								contentVersion += 1;
								schedulePreview();
							}}
							onsave={save}
							onunauthorized={() => goto('/login')}
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
						<span class="kbd">Ctrl</span> + <span class="kbd">S</span> saves while editing.
					</p>
				</div>
			</div>
		{/if}
	</main>

	<aside class="rail">
		<div class="rail-tabs">
			<button class="tab" class:active={railTab === 'links'} on:click={() => (railTab = 'links')}>
				Links
			</button>
			<button class="tab" class:active={railTab === 'chat'} on:click={() => (railTab = 'chat')}>
				Chat
			</button>
		</div>

		{#if railTab === 'links'}
			{#if selected}
				<h3>Outgoing links</h3>
				{#each outlinks as link (link.target)}
					{#if link.exists && link.path}
						<button class="link-item" on:click={() => openDoc(link.path!)}>
							{link.target}
						</button>
					{:else}
						<button class="link-item missing" disabled>{link.target} · missing</button>
					{/if}
				{:else}
					<span class="empty">No links yet. Type <code>[[</code> to link a document.</span>
				{/each}

				<h3>Backlinks</h3>
				{#each backlinks as link (link.path)}
					<button class="link-item" on:click={() => openDoc(link.path)}>
						{link.title || link.path}
					</button>
				{:else}
					<span class="empty">No documents link here yet.</span>
				{/each}
			{:else}
				<h3>Vault</h3>
				<span class="empty">{docs.length} document{docs.length === 1 ? '' : 's'} indexed.</span>
			{/if}
		{:else}
			<h3>Chat</h3>
			<div class="agent-log">
				{#each agentMessages as msg (msg.id)}
					<div class="msg" class:user={msg.role === 'user'}>
						<div class="msg-text">{msg.content}</div>
					</div>
				{:else}
					<span class="empty">Ask about your vault.</span>
				{/each}
				{#if agentError}
					<div class="banner" style="margin: 8px 0 0;">{agentError}</div>
				{/if}
			</div>
			<div class="agent-input">
				<textarea
					rows="2"
					placeholder="Ask the vault…"
					bind:value={agentInput}
					on:keydown={(e) => {
						if (e.key === 'Enter' && !e.shiftKey) {
							e.preventDefault();
							sendAgent();
						}
					}}
				></textarea>
				{#if agentBusy}
					<button class="btn" on:click={stopAgent}>Stop</button>
				{:else}
					<button class="btn primary" on:click={sendAgent} disabled={!agentInput.trim()}>
						Send
					</button>
				{/if}
			</div>
		{/if}
	</aside>
</div>
