<script lang="ts">
	import type { AgentToolStep } from '$lib/agent';

	export let tool: AgentToolStep;
	let open = false;

	const icons: Record<string, string> = {
		search_documents: '⌕',
		search_files: '⌕',
		read_document: '▤',
		list_documents: '☰',
		propose_edit: '✎',
		append_to_document: '＋',
		replace_document: '↻',
		create_document: '✚'
	};
</script>

<div class="agent-tool" class:running={tool.state === 'start'} class:error={tool.state === 'error'}>
	<span class="agent-tool-icon" aria-hidden="true">{icons[tool.name] ?? '·'}</span>
	<div class="agent-tool-body">
		<div class="agent-tool-line">
			<span class="agent-tool-summary">{tool.summary}</span>
			{#if tool.state === 'start'}
				<span class="agent-tool-spinner" aria-label="running"></span>
			{:else if tool.state === 'done'}
				<span class="agent-tool-check" aria-label="done">✓</span>
			{:else if tool.state === 'error'}
				<span class="agent-tool-warn" aria-label="failed">!</span>
			{/if}
			{#if tool.detail}
				<button class="agent-tool-toggle" on:click={() => (open = !open)}>
					{open ? 'Hide' : 'Details'}
				</button>
			{/if}
		</div>
		{#if open && tool.detail}
			<pre class="agent-tool-detail">{tool.detail}</pre>
		{/if}
	</div>
</div>
