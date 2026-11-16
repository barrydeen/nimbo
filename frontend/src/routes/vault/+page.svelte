<script lang="ts">
	import { onMount } from 'svelte';
	import { goto, replaceState } from '$app/navigation';
	import { api, type DocMeta, type SearchResult, type User } from '$lib/api';
	import Editor from '$lib/components/Editor.svelte';
	import { marked } from 'marked';

	let user: User | null = null;
	let instanceName = 'Nimbo';

	let docs: DocMeta[] = [];
	let selected: string | null = null;
	let content = '';
	let hash = '';
	let dirty = false;
	let saving = false;
	let split = true;
	let aiEnabled = false;
	let editorKey = 0;

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

	onMount(async () => {
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
			await refresh();
			const path = new URLSearchParams(location.search).get('path');
			if (path) {
				await openDoc(path);
			}
		} catch {
			goto('/login');
		}
	});

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

	function message(err: unknown): string {
		return err instanceof Error ? err.message : 'Something went wrong';
	}

	async function openDoc(path: string) {
		if (dirty && !confirm('Discard unsaved changes?')) return;
		try {
			const doc = await api.getDoc(path);
			selected = path;
			content = doc.content;
			hash = doc.hash;
			dirty = false;
			editorKey += 1;
			errorMsg = '';
			searchMode = false;
			replaceState(`?path=${encodeURIComponent(path)}`, {});
			const connections = await api.backlinks(path);
			backlinks = connections.backlinks;
			outlinks = connections.links;
		} catch (err) {
			errorMsg = message(err);
		}
	}

	async function save() {
		if (!selected || !dirty) return;
		saving = true;
		errorMsg = '';
		try {
			const res = await api.saveDoc({ path: selected, content, base_hash: hash });
			hash = res.hash;
			dirty = false;
			statusMsg = 'Saved';
			setTimeout(() => (statusMsg = ''), 1500);
			await refresh();
			const connections = await api.backlinks(selected);
			backlinks = connections.backlinks;
			outlinks = connections.links;
		} catch (err) {
			errorMsg = message(err);
		} finally {
			saving = false;
		}
	}

	async function createDoc() {
		const raw = newPath.trim();
		if (!raw) return;
		const path = raw.endsWith('.md') ? raw : `${raw}.md`;
		const title = path.split('/').pop()?.replace(/\.md$/, '') ?? 'Untitled';
		try {
			await api.saveDoc({ path, content: `# ${title}\n\n`, base_hash: null });
			newPath = '';
			showNew = false;
			await refresh();
			await openDoc(path);
		} catch (err) {
			errorMsg = message(err);
		}
	}

	async function renameDoc() {
		if (!selected) return;
		const next = prompt('New path', selected);
		if (!next || next === selected) return;
		try {
			const res = await api.renameDoc(selected, next);
			dirty = false;
			await refresh();
			await openDoc(res.path);
		} catch (err) {
			errorMsg = message(err);
		}
	}

	async function deleteDoc() {
		if (!selected) return;
		if (!confirm(`Delete ${selected}? This removes the file from disk.`)) return;
		try {
			await api.deleteDoc(selected);
			selected = null;
			content = '';
			hash = '';
			dirty = false;
			backlinks = [];
			outlinks = [];
			replaceState('', {});
			await refresh();
		} catch (err) {
			errorMsg = message(err);
		}
	}

	async function runSearch() {
		const q = query.trim();
		errorMsg = '';
		if (!q) {
			results = [];
			searchMode = false;
			return;
		}
		searching = true;
		searchMode = true;
		try {
			const res = await api.search(q);
			results = res.results;
		} catch (err) {
			errorMsg = message(err);
		} finally {
			searching = false;
		}
	}

	function clearSearch() {
		query = '';
		results = [];
		searchMode = false;
	}

	function resolveWikiTarget(target: string): string | null {
		const wanted = target.trim();
		if (!wanted) return null;
		const withExt = wanted.endsWith('.md') ? wanted : `${wanted}.md`;
		const lower = withExt.toLowerCase();
		const exact = docs.find((d) => d.path.toLowerCase() === lower);
		if (exact) return exact.path;
		const byName = docs.find(
			(d) => d.path.toLowerCase() === lower || d.path.toLowerCase().endsWith(`/${lower}`)
		);
		return byName ? byName.path : null;
	}

	function openWiki(target: string) {
		const path = resolveWikiTarget(target);
		if (path) {
			openDoc(path);
		} else {
			errorMsg = `No document named "${target}"`;
		}
	}

	function renderMarkdown(md: string): string {
		const linked = md.replace(
			/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g,
			(_match, target: string, alias: string | undefined) => {
				const label = (alias || target).trim();
				return `<a href="#" class="wikilink" data-wiki="${encodeURIComponent(target.trim())}">${label}</a>`;
			}
		);
		return marked.parse(linked, { async: false }) as string;
	}

	function onPreviewClick(event: MouseEvent) {
		const anchor = (event.target as HTMLElement).closest('a[data-wiki]') as HTMLAnchorElement | null;
		if (!anchor) return;
		event.preventDefault();
		openWiki(decodeURIComponent(anchor.dataset.wiki || ''));
	}

	async function logout() {
		try {
			await api.logout();
		} finally {
			goto('/login');
		}
	}

	async function sendAgent() {
		const userMessage = agentInput.trim();
		if (!userMessage || agentBusy) return;
		agentInput = '';
		agentError = '';
		agentBusy = true;
		agentMessages = [
			...agentMessages,
			{ id: ++msgSeq, role: 'user', content: userMessage },
			{ id: ++msgSeq, role: 'assistant', content: '' }
		];
		const index = agentMessages.length - 1;

		try {
			const res = await fetch('/api/agent/chat', {
				method: 'POST',
				credentials: 'include',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ message: userMessage, path: selected })
			});
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
				buffer += decoder.decode(value, { stream: true });
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
						/* finished */
					} else if (data) {
						agentMessages = agentMessages.map((m, i) =>
							i === index ? { ...m, content: m.content + data } : m
						);
					}
				}
			}
		} catch (err) {
			agentError = message(err);
		} finally {
			agentBusy = false;
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
		<button class="btn ghost" on:click={() => goto('/drive')}>Files</button>
		{#if user?.role === 'admin'}
			<button class="btn ghost" on:click={() => goto('/settings')}>Settings</button>
		{/if}
		<button class="btn ghost" on:click={logout}>Sign out</button>
	</header>

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
							<div class="snip">{@html result.snippet}</div>
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
			<div class="banner" style="margin: 10px 14px 0;">{errorMsg}</div>
		{/if}

		{#if selected}
			<div class="toolbar">
				<span class="path">{selected}</span>
				{#if dirty}<span class="dot dirty" title="Unsaved changes"></span>{/if}
				{#if statusMsg}<span class="path" style="color: var(--good)">{statusMsg}</span>{/if}
				<div class="spacer"></div>
				<button class="btn" class:active={split} on:click={() => (split = !split)}>
					{split ? 'Single pane' : 'Split preview'}
				</button>
				<button class="btn primary" on:click={save} disabled={!dirty || saving}>
					{saving ? 'Saving…' : 'Save'}
				</button>
				<button class="btn ghost" on:click={renameDoc}>Rename</button>
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
							}}
							onsave={save}
						/>
					{/key}
				</div>
				{#if split}
					<div class="preview-pane">
						<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
						<div class="preview" on:click={onPreviewClick}>
							<div class="markdown-body">{@html renderMarkdown(content)}</div>
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
				<button class="btn primary" on:click={sendAgent} disabled={agentBusy || !agentInput.trim()}>
					{agentBusy ? '…' : 'Send'}
				</button>
			</div>
		{/if}
	</aside>
</div>
