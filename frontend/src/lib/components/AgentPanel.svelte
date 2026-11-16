<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { api, errorMessage, type DocMeta, type User } from '$lib/api';
	import {
		AgentSseParser,
		clearConversation,
		loadConversation,
		selectedEdits,
		saveConversation,
		type AgentDraft,
		type AgentDraftEdit,
		type AgentDraftPart,
		type AgentMessage,
		type AgentMode,
		type AgentToolStep,
		type DraftStatus
	} from '$lib/agent';
	import AgentMarkdown from './AgentMarkdown.svelte';
	import AgentToolStepView from './AgentToolStep.svelte';
	import AgentDraftView from './AgentDraft.svelte';
	import AgentComposer from './AgentComposer.svelte';

	export let path: string | null = null;
	export let docContent = '';
	export let docHash = '';
	export let docVersion = 0;
	export let dirty = false;
	export let selection: { text: string; from: number; to: number } | null = null;
	export let docs: DocMeta[] = [];
	export let user: User | null = null;
	export let backlinks: { path: string; title: string }[] = [];
	export let outlinks: { target: string; path: string | null; exists: boolean }[] = [];
	export let onapplydraft: (
		draft: AgentDraft,
		edits: AgentDraftEdit[]
	) => Promise<{ ok: boolean; error?: string; warning?: string }> = async () => ({
		ok: false,
		error: 'Apply is unavailable.'
	});
	export let onundo: (draft: AgentDraft) => Promise<{ ok: boolean; error?: string }> = async () => ({
		ok: false,
		error: 'Undo is unavailable.'
	});
	export let onopendoc: (path: string) => void = () => {};
	export let onnotify: (message: string) => void = () => {};
	export let onconfigure: () => void = () => {};
	export let open = false;
	export let onclose: () => void = () => {};

	let messages: AgentMessage[] = [];
	let mode: AgentMode = 'auto';
	let input = '';
	let busy = false;
	let error = '';
	let statusLabel = '';
	let aiConfigured: boolean | null = null;
	let conversationKey = 'vault';
	let lastPath: string | null = null;
	let runId = 0;
	let abortController: AbortController | null = null;
	let msgSeq = 0;
	let logEl: HTMLDivElement;
	let atBottom = true;
	let contextOpen = false;
	let mounted = false;
	let composer: AgentComposer;
	let persistTimer: ReturnType<typeof setTimeout> | null = null;
	let scrollFrame: number | null = null;

	$: if (mounted && path !== lastPath) {
		lastPath = path;
		void switchConversation(path);
	}

	$: pendingCount = messages.reduce(
		(count, message) =>
			count +
			message.parts.filter((part) => part.type === 'draft' && part.status === 'pending').length,
		0
	);

	$: if (mounted) {
		messages;
		mode;
		schedulePersist();
	}

	function normalizeMessages(stored: AgentMessage[]): AgentMessage[] {
		return stored.map((message) => ({
			...message,
			streaming: false,
			parts: message.parts.map((part) => {
				if (part.type !== 'draft') return part;
				const length = part.draft.edits?.length ?? 0;
				const selected = Array.isArray(part.selected)
					? part.selected.slice(0, length)
					: new Array<boolean>(length).fill(true);
				while (selected.length < length) selected.push(true);
				return {
					...part,
					status: part.status === 'applying' ? 'pending' : part.status,
					selected
				};
			})
		}));
	}

	function load(key: string) {
		const stored = loadConversation(key);
		messages = normalizeMessages(stored?.messages ?? []);
		mode = stored?.mode ?? 'auto';
		msgSeq = messages.reduce((max, message) => Math.max(max, message.id), 0);
	}

	function persist(key = conversationKey) {
		saveConversation(key, { messages, mode });
	}

	function schedulePersist() {
		if (persistTimer) clearTimeout(persistTimer);
		persistTimer = setTimeout(() => {
			persistTimer = null;
			persist();
		}, 400);
	}

	async function switchConversation(next: string | null) {
		abortController?.abort();
		abortController = null;
		busy = false;
		runId += 1;
		persist(conversationKey);
		conversationKey = next ?? 'vault';
		load(conversationKey);
		error = '';
		statusLabel = '';
		atBottom = true;
		await tick();
		void scrollToBottom(true);
	}

	async function checkConfigured() {
		try {
			const settings = await api.getLlm();
			aiConfigured = settings.configured;
		} catch {
			aiConfigured = null;
		}
	}

	function onScroll() {
		if (!logEl) return;
		atBottom = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 80;
	}

	async function scrollToBottom(force = false) {
		if (!force && !atBottom) return;
		await tick();
		if (!logEl) return;
		logEl.scrollTop = logEl.scrollHeight;
		requestAnimationFrame(() => {
			if (logEl) logEl.scrollTop = logEl.scrollHeight;
		});
	}

	function scheduleScroll() {
		if (scrollFrame !== null) return;
		scrollFrame = requestAnimationFrame(() => {
			scrollFrame = null;
			void scrollToBottom();
		});
	}

	function buildHistory() {
		const history: { role: 'user' | 'assistant'; content: string }[] = [];
		for (const message of messages) {
			const text = message.parts
				.filter((part) => part.type === 'text')
				.map((part) => part.text)
				.join('\n')
				.trim();
			if (!text) continue;
			history.push({ role: message.role, content: text.slice(0, 8_000) });
		}
		return history.slice(-12);
	}

	function draftSummaries() {
		const drafts: {
			id: string;
			kind: string;
			path: string;
			status: DraftStatus;
			rationale: string;
		}[] = [];
		for (const message of messages) {
			for (const part of message.parts) {
				if (part.type !== 'draft') continue;
				drafts.push({
					id: part.draft.id,
					kind: part.draft.kind,
					path: part.draft.path,
					status: part.status,
					rationale: part.draft.rationale
				});
			}
		}
		return drafts.slice(-8);
	}

	function appendText(message: AgentMessage, text: string) {
		if (!text) return;
		const last = message.parts[message.parts.length - 1];
		if (last && last.type === 'text') last.text += text;
		else message.parts.push({ type: 'text', text });
		messages = [...messages];
	}

	function upsertTool(message: AgentMessage, tool: AgentToolStep) {
		const existing = message.parts.find(
			(part): part is Extract<typeof part, { type: 'tool' }> =>
				part.type === 'tool' && part.tool.id === tool.id
		);
		if (existing) existing.tool = tool;
		else message.parts.push({ type: 'tool', tool });
		messages = [...messages];
	}

	function appendDraft(message: AgentMessage, draft: AgentDraft) {
		const existing = message.parts.find(
			(part): part is AgentDraftPart => part.type === 'draft' && part.draft.id === draft.id
		);
		if (existing) {
			existing.draft = draft;
		} else {
			message.parts.push({
				type: 'draft',
				draft,
				status: 'pending',
				selected: (draft.edits ?? []).map(() => true)
			});
		}
		messages = [...messages];
	}

	function applyEvent(assistantId: number, event: ReturnType<AgentSseParser['push']>[number]) {
		const message = messages.find((entry) => entry.id === assistantId);
		if (!message) return;
		switch (event.type) {
			case 'status':
				statusLabel = event.label;
				break;
			case 'text':
				statusLabel = '';
				appendText(message, event.text);
				break;
			case 'tool':
				upsertTool(message, event.tool);
				statusLabel = '';
				break;
			case 'draft':
				appendDraft(message, event.draft);
				statusLabel = 'Draft ready';
				break;
			case 'error':
				message.error = event.message;
				error = event.message;
				break;
			case 'done':
				statusLabel = '';
				break;
		}
	}

	function finalizeAssistant(assistantId: number) {
		const message = messages.find((entry) => entry.id === assistantId);
		if (!message) return;
		message.streaming = false;
		if (!message.parts.length && !message.error) {
			message.parts.push({
				type: 'text',
				text: 'No response. Try again or switch the assistant mode.'
			});
		}
		messages = [...messages];
	}

	function markStopped(assistantId: number) {
		const message = messages.find((entry) => entry.id === assistantId);
		if (!message) return;
		message.streaming = false;
		message.stopped = true;
		if (!message.parts.length) message.parts.push({ type: 'text', text: 'Stopped.' });
		messages = [...messages];
	}

	function markError(assistantId: number, messageText: string) {
		const message = messages.find((entry) => entry.id === assistantId);
		if (!message) return;
		message.streaming = false;
		message.error = messageText;
		messages = [...messages];
	}

	async function send(text?: string, forcedMode?: AgentMode) {
		const userText = (text ?? input).trim();
		if (!userText || busy || aiConfigured === false) return;
		const sendMode = forcedMode ?? mode;
		if (text === undefined) input = '';
		error = '';
		statusLabel = 'Thinking…';

		const history = buildHistory();
		const drafts = draftSummaries();
		const userMessage: AgentMessage = {
			id: ++msgSeq,
			role: 'user',
			parts: [{ type: 'text', text: userText }]
		};
		const assistantId = ++msgSeq;
		const assistant: AgentMessage = { id: assistantId, role: 'assistant', parts: [], streaming: true };
		messages = [...messages, userMessage, assistant];
		atBottom = true;
		await tick();
		void scrollToBottom(true);

		const controller = new AbortController();
		abortController = controller;
		const run = ++runId;
		busy = true;
		try {
			const res = await fetch('/api/agent/chat', {
				method: 'POST',
				credentials: 'include',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({
					message: userText,
					path,
					doc: { content: docContent, hash: docHash, dirty, version: docVersion },
					selection: selection
						? {
								text: selection.text.slice(0, 8_000),
								from: selection.from,
								to: selection.to
							}
						: null,
					history,
					mode: sendMode,
					drafts
				}),
				signal: controller.signal
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
			if (!reader) throw new Error('Streaming is not supported in this browser.');
			const decoder = new TextDecoder();
			const parser = new AgentSseParser();
			while (true) {
				const { done, value } = await reader.read();
				if (done) break;
				if (run !== runId || controller.signal.aborted) {
					await reader.cancel().catch(() => undefined);
					break;
				}
				for (const event of parser.push(decoder.decode(value, { stream: true }))) {
					if (run !== runId) break;
					applyEvent(assistantId, event);
				}
				scheduleScroll();
			}
			for (const event of parser.finish()) applyEvent(assistantId, event);
			finalizeAssistant(assistantId);
		} catch (err) {
			if (controller.signal.aborted) {
				markStopped(assistantId);
			} else {
				const messageText = errorMessage(err);
				error = messageText;
				markError(assistantId, messageText);
			}
		} finally {
			if (run === runId) {
				busy = false;
				abortController = null;
				statusLabel = '';
				finalizeAssistant(assistantId);
				persist();
			}
		}
		composer?.focus();
	}

	function stop() {
		abortController?.abort();
		busy = false;
		statusLabel = '';
	}

	async function newChat() {
		if (!confirm('Clear this conversation?')) return;
		messages = [];
		error = '';
		statusLabel = '';
		clearConversation(conversationKey);
		persist();
		await tick();
		composer?.focus();
	}

	function toggleDraft(part: AgentDraftPart, index: number) {
		part.selected = part.selected.map((value, current) =>
			current === index ? !value : value
		);
		messages = [...messages];
		persist();
	}

	async function applyDraft(part: AgentDraftPart) {
		const edits = selectedEdits(part);
		if (part.draft.kind === 'edit' && edits.length === 0) {
			part.error = 'Select at least one change to apply.';
			messages = [...messages];
			return;
		}
		part.status = 'applying';
		part.error = undefined;
		messages = [...messages];
		try {
			const result = await onapplydraft(part.draft, edits);
			if (result.ok) {
				part.status = 'applied';
				onnotify(result.warning || 'Draft applied');
			} else {
				part.status = 'error';
				part.error = result.error || 'The draft could not be applied.';
			}
		} catch (err) {
			part.status = 'error';
			part.error = errorMessage(err);
		}
		messages = [...messages];
		persist();
		void scrollToBottom(true);
	}

	function discardDraft(part: AgentDraftPart) {
		part.status = 'discarded';
		part.error = undefined;
		part.selected = part.selected.map(() => false);
		messages = [...messages];
		persist();
		void scrollToBottom(true);
	}

	function regenerateDraft(part: AgentDraftPart) {
		part.status = 'discarded';
		messages = [...messages];
		const target = part.draft.path;
		void send(
			`Please regenerate the ${part.draft.kind} draft for ${target} against the current document content.`,
			'edit'
		);
	}

	function openDraft(part: AgentDraftPart) {
		onopendoc(part.draft.path);
	}

	async function undoDraft(part: AgentDraftPart) {
		if (part.draft.path !== path) {
			onnotify('Open the document to undo this change.');
			return;
		}
		const result = await onundo(part.draft);
		if (result.ok) {
			part.status = 'pending';
			part.error = undefined;
			part.selected = (part.draft.edits ?? []).map(() => true);
			onnotify('Change undone');
		} else {
			part.error = result.error || 'Undo failed.';
		}
		messages = [...messages];
		persist();
		void scrollToBottom(true);
	}

	function quick(label: string, quickMode: AgentMode) {
		const prompts: Record<string, string> = {
			Summarize: 'Summarize this document.',
			'Find related': 'Find related notes in the vault.',
			'Fix grammar':
				'Fix the grammar and clarity of this document. Propose an edit with the exact changes.',
			Rewrite: selection?.text
				? 'Rewrite the selected text to be clearer and more concise. Propose an edit.'
				: 'Rewrite this document to be clearer and more concise. Propose an edit.',
			'Add links':
				'Suggest useful wikilinks for this document and propose an edit that adds them.'
		};
		void send(prompts[label] || label, quickMode);
	}

	function draftIt() {
		void send('Turn your last suggestion into a draft for this document.', 'edit');
	}

	onMount(() => {
		mounted = true;
		lastPath = path;
		conversationKey = path ?? 'vault';
		load(conversationKey);
		void checkConfigured();
		void tick().then(() => scrollToBottom(true));
	});

	onDestroy(() => {
		mounted = false;
		abortController?.abort();
		persist(conversationKey);
		if (persistTimer) clearTimeout(persistTimer);
		if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
	});
</script>

<aside class="rail agent-panel" class:open>
	<header class="agent-header">
		<div class="agent-brand">
			<span class="agent-mark" aria-hidden="true">✦</span>
			<div>
				<strong>Nimbo Assistant</strong>
				<span class="agent-sub">{path ? 'This document' : 'Vault-wide'}</span>
			</div>
		</div>
		<div class="agent-header-actions">
			{#if pendingCount > 0}
				<span class="agent-pending" title="Pending drafts">{pendingCount}</span>
			{/if}
			<button class="btn ghost" on:click={() => (contextOpen = !contextOpen)}>
				Context
			</button>
			{#if user?.role === 'admin'}
				<button class="btn ghost" on:click={onconfigure} title="LLM settings">Settings</button>
			{/if}
			<button class="btn ghost" on:click={newChat} disabled={!messages.length}>New chat</button>
			<button class="agent-close" aria-label="Close assistant" on:click={onclose}>×</button>
		</div>
	</header>

	{#if contextOpen}
		<div class="agent-context-card">
			{#if path}
				<div class="agent-context-row">
					<span class="agent-context-path">{path}</span>
					{#if dirty}<span class="agent-dirty">unsaved</span>{/if}
				</div>
				{#if selection?.text}
					<div class="agent-context-row">
						<span class="agent-context-chip">Selection · {selection.text.length} chars</span>
					</div>
				{/if}
				<div class="agent-context-links">
					<div>
						<span class="agent-context-label">Outlinks</span>
						{#each outlinks.slice(0, 8) as link (link.target)}
							<button
								class="agent-context-link"
								disabled={!link.path}
								on:click={() => link.path && onopendoc(link.path)}
							>
								{link.target}{#if !link.exists}<span class="agent-missing"> · missing</span>{/if}
							</button>
						{:else}
							<span class="agent-context-empty">None</span>
						{/each}
					</div>
					<div>
						<span class="agent-context-label">Backlinks</span>
						{#each backlinks.slice(0, 8) as link (link.path)}
							<button class="agent-context-link" on:click={() => onopendoc(link.path)}>
								{link.title || link.path}
							</button>
						{:else}
							<span class="agent-context-empty">None</span>
						{/each}
					</div>
				</div>
			{:else}
				<div class="agent-context-row">
					<span class="agent-context-path">Vault</span>
					<span class="agent-context-empty">{docs.length} documents</span>
				</div>
			{/if}
		</div>
	{/if}

	<div class="agent-log" bind:this={logEl} on:scroll={onScroll} role="log">
		{#if messages.length === 0}
			<div class="agent-empty">
				<div class="agent-empty-mark" aria-hidden="true">✦</div>
				<h3>{path ? 'Ask about this note' : 'Ask your vault'}</h3>
				<p>
					Chat about the document, or ask for a change. Edits arrive as drafts you can review
					before anything is saved.
				</p>
				<div class="agent-empty-actions">
					{#if path}
						<button class="agent-quick-chip" on:click={() => quick('Summarize', 'ask')}>
							Summarize this note
						</button>
						<button class="agent-quick-chip" on:click={() => quick('Fix grammar', 'edit')}>
							Fix grammar
						</button>
					{/if}
					<button class="agent-quick-chip" on:click={() => quick('Find related', 'ask')}>
						Find related notes
					</button>
				</div>
			</div>
		{:else}
			{#each messages as message (message.id)}
				<div class="agent-msg" class:user={message.role === 'user'}>
					{#if message.role === 'assistant'}<div class="agent-avatar" aria-hidden="true">✦</div>{/if}
					<div class="agent-bubble">
						{#each message.parts as part, index (index)}
							{#if part.type === 'text'}
								<AgentMarkdown
									text={part.text}
									streaming={!!message.streaming && index === message.parts.length - 1}
									{onopendoc}
								/>
							{:else if part.type === 'tool'}
								<AgentToolStepView tool={part.tool} />
							{:else if part.type === 'draft'}
								<AgentDraftView
									draft={part.draft}
									status={part.status}
									error={part.error}
									selected={part.selected}
									currentContent={part.draft.path === path ? docContent : ''}
									stale={!!part.draft.base_version && part.draft.base_version !== docVersion}
									onapply={() => applyDraft(part)}
									ondiscard={() => discardDraft(part)}
									onopen={() => openDraft(part)}
									onregenerate={() => regenerateDraft(part)}
									ontoggle={(index) => toggleDraft(part, index)}
									onundo={() => undoDraft(part)}
								/>
							{/if}
						{/each}
						{#if message.streaming && statusLabel}
							<div class="agent-status" aria-live="polite">{statusLabel}</div>
						{/if}
						{#if message.streaming && !message.parts.length}
							<div class="agent-typing" aria-label="Thinking">
								<span></span><span></span><span></span>
							</div>
						{/if}
						{#if message.error}
							<div class="agent-msg-error" role="alert">{message.error}</div>
						{/if}
						{#if message.role === 'assistant' && !message.streaming && !message.parts.some((part) => part.type === 'draft')}
							<div class="agent-msg-actions">
								<button class="agent-link" on:click={draftIt}>Draft it</button>
							</div>
						{/if}
					</div>
				</div>
			{/each}
		{/if}
		{#if error}
			<div class="banner agent-banner" role="alert">
				<span>{error}</span>
				<button class="btn ghost" on:click={() => (error = '')}>Dismiss</button>
			</div>
		{/if}
	</div>

	{#if !atBottom && messages.length > 0}
		<button class="agent-jump" on:click={() => scrollToBottom(true)}>Jump to latest</button>
	{/if}

	<AgentComposer
		bind:this={composer}
		bind:value={input}
		bind:mode
		{busy}
		disabled={aiConfigured === false}
		hasSelection={!!selection?.text}
		hasDocument={!!path}
		canConfigure={user?.role === 'admin'}
		onSend={() => send()}
		onStop={stop}
		onMode={(next) => (mode = next)}
		onQuick={(label, quickMode) => quick(label, quickMode)}
		onConfigure={user?.role === 'admin'
			? onconfigure
			: () => onnotify('Ask an administrator to configure the assistant.')}
	/>
</aside>
