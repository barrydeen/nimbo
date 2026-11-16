<script lang="ts">
	import { lineDiff, type AgentDraft, type DiffLine, type DraftStatus } from '$lib/agent';

	export let draft: AgentDraft;
	export let status: DraftStatus = 'pending';
	export let error = '';
	export let selected: boolean[] = [];
	export let currentContent = '';
	export let stale = false;
	export let onapply: () => void = () => {};
	export let ondiscard: () => void = () => {};
	export let onopen: () => void = () => {};
	export let onregenerate: () => void = () => {};
	export let ontoggle: (index: number) => void = () => {};
	export let onundo: () => void = () => {};

	let showAll = false;

	$: edits = draft.edits ?? [];
	$: selectedCount = edits.filter((_, index) => selected[index] !== false).length;
	$: canApply =
		status === 'pending' &&
		(draft.kind === 'create' ||
			draft.kind === 'append' ||
			draft.kind === 'replace' ||
			selectedCount > 0);
	$: label =
		draft.kind === 'create'
			? 'Create document'
			: draft.kind === 'append'
				? 'Append'
				: draft.kind === 'replace'
					? 'Replace document'
					: `Apply ${selectedCount} change${selectedCount === 1 ? '' : 's'}`;

	type Hunk = { key: string; lines: DiffLine[]; truncated: boolean };

	$: hunks = buildHunks(draft, currentContent, showAll);
	$: contentPreview = previewText(draft, showAll);

	function buildHunks(draft: AgentDraft, currentContent: string, showAll: boolean): Hunk[] {
		if (draft.kind === 'create' || draft.kind === 'append') return [];
		if (draft.kind === 'replace') {
			const lines = lineDiff(currentContent, draft.content ?? '', 1);
			return [
				{
					key: 'full',
					lines: showAll ? lines : lines.slice(0, 200),
					truncated: lines.length > 200
				}
			];
		}
		return (draft.edits ?? []).map((edit, index) => {
			const lines = lineDiff(edit.find, edit.replace, edit.start_line ?? 1);
			return {
				key: `edit-${index}`,
				lines: showAll ? lines : lines.slice(0, 120),
				truncated: lines.length > 120
			};
		});
	}

	function previewText(draft: AgentDraft, showAll: boolean): { text: string; truncated: boolean } {
		if (draft.kind !== 'create' && draft.kind !== 'append') {
			return { text: '', truncated: false };
		}
		const content = draft.content ?? '';
		const lines = content.split('\n');
		const limit = showAll ? lines.length : 120;
		return {
			text: lines.slice(0, limit).join('\n'),
			truncated: lines.length > limit
		};
	}
</script>

<div class="agent-draft" class:applied={status === 'applied'} class:discarded={status === 'discarded'} class:failed={status === 'error'}>
	<div class="agent-draft-head">
		<span class="agent-draft-kind">
			{draft.kind === 'create'
				? 'New document'
				: draft.kind === 'append'
					? 'Proposed append'
					: draft.kind === 'replace'
						? 'Proposed rewrite'
						: 'Proposed edit'}
		</span>
		<span class="agent-draft-path">{draft.path}</span>
		{#if stale && status === 'pending'}<span class="agent-draft-stale">Document changed</span>{/if}
		{#if status === 'applied'}<span class="agent-draft-done">Applied</span>{/if}
		{#if status === 'discarded'}<span class="agent-draft-discarded">Discarded</span>{/if}
	</div>

	{#if draft.rationale}
		<p class="agent-draft-rationale">{draft.rationale}</p>
	{/if}

	{#if draft.kind === 'edit' || draft.kind === 'replace'}
		<div class="agent-hunks">
			{#each hunks as hunk, index (hunk.key)}
				<div class="agent-hunk">
					{#if draft.kind === 'edit'}
						<label class="agent-hunk-check">
							<input
								type="checkbox"
								checked={selected[index] !== false}
								disabled={status !== 'pending'}
								on:change={() => ontoggle(index)}
							/>
							<span>Change {index + 1}</span>
						</label>
					{/if}
					<div class="agent-diff" role="table" aria-label="Proposed difference">
						{#each hunk.lines as line, lineIndex (lineIndex)}
							<div class="agent-diff-line {line.kind}" role="row">
								<span class="agent-diff-gutter" aria-hidden="true">
									{line.kind === 'del' ? line.oldLine ?? '' : line.newLine ?? line.oldLine ?? ''}
								</span>
								<span class="agent-diff-sign" aria-hidden="true">
									{line.kind === 'add' ? '+' : line.kind === 'del' ? '−' : ' '}
								</span>
								<span class="agent-diff-text">{line.text}</span>
							</div>
						{/each}
					</div>
					{#if hunk.truncated && !showAll}
						<button class="agent-link" on:click={() => (showAll = true)}>
							Show the full difference
						</button>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<div class="agent-diff agent-diff-plain">
			{#each contentPreview.text.split('\n') as line, index (index)}
				<div class="agent-diff-line add" role="row">
					<span class="agent-diff-gutter" aria-hidden="true">{index + 1}</span>
					<span class="agent-diff-sign" aria-hidden="true">+</span>
					<span class="agent-diff-text">{line}</span>
				</div>
			{/each}
		</div>
		{#if contentPreview.truncated && !showAll}
			<button class="agent-link" on:click={() => (showAll = true)}>Show all content</button>
		{/if}
	{/if}

	{#if error}
		<div class="agent-draft-error" role="alert">{error}</div>
	{/if}

	<div class="agent-draft-actions">
		{#if status === 'pending'}
			<button class="btn primary agent-apply" on:click={onapply} disabled={!canApply}>
				{label}
			</button>
			<button class="btn ghost" on:click={ondiscard}>Discard</button>
		{:else if status === 'applying'}
			<button class="btn primary agent-apply" disabled>Saving…</button>
		{:else if status === 'applied'}
			<button class="btn ghost" on:click={onopen}>Open {draft.path.split('/').pop()}</button>
			{#if draft.kind !== 'create'}
				<button class="btn ghost" on:click={onundo}>Undo</button>
			{/if}
		{:else if status === 'error' || status === 'discarded'}
			<button class="btn ghost" on:click={onregenerate}>Regenerate</button>
			{#if draft.kind !== 'create'}
				<button class="btn ghost" on:click={onopen}>Open</button>
			{/if}
		{/if}
		{#if status === 'pending' && draft.path && draft.kind !== 'create'}
			<button class="btn ghost" on:click={onopen}>Open</button>
		{/if}
	</div>
</div>
