<script lang="ts">
	import type { AgentMode } from '$lib/agent';

	export let value = '';
	export let mode: AgentMode = 'auto';
	export let busy = false;
	export let disabled = false;
	export let hasSelection = false;
	export let hasDocument = false;
	export let canConfigure = false;
	export let onSend: () => void = () => {};
	export let onStop: () => void = () => {};
	export let onMode: (mode: AgentMode) => void = () => {};
	export let onQuick: (label: string, mode: AgentMode) => void = () => {};
	export let onConfigure: () => void = () => {};

	let textarea: HTMLTextAreaElement;

	export function focus() {
		textarea?.focus();
	}

	const quickActions: { label: string; mode: AgentMode; needsSelection?: boolean }[] = [
		{ label: 'Summarize', mode: 'ask' },
		{ label: 'Find related', mode: 'ask' },
		{ label: 'Fix grammar', mode: 'edit' },
		{ label: 'Rewrite', mode: 'edit', needsSelection: true },
		{ label: 'Add links', mode: 'edit' }
	];

	$: visibleActions = quickActions.filter((action) => {
		if (action.needsSelection && !hasSelection) return false;
		if (!hasDocument && action.label !== 'Find related') return false;
		return true;
	});
	$: placeholder = disabled
		? 'Connect a model in Settings to use the assistant.'
		: hasDocument
			? 'Ask about this note, or ask for a change…'
			: 'Ask the vault…';

	$: if (!value && textarea) textarea.style.height = 'auto';

	function grow() {
		if (!textarea) return;
		textarea.style.height = 'auto';
		textarea.style.height = `${Math.min(textarea.scrollHeight, 180)}px`;
	}

	function keydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			if (!busy && value.trim() && !disabled) onSend();
		}
	}
</script>

<div class="agent-composer">
	{#if disabled}
		<div class="agent-composer-off">
			<span>The assistant is not configured.</span>
			{#if canConfigure}
				<button class="btn primary" on:click={onConfigure}>Open Settings</button>
			{:else}
				<span class="agent-context-empty">Ask an administrator to configure it.</span>
			{/if}
		</div>
	{:else}
		<div class="agent-quick">
			{#each visibleActions as action (action.label)}
				<button
					class="agent-quick-chip"
					disabled={busy}
					on:click={() => onQuick(action.label, action.mode)}
				>
					{action.label}
				</button>
			{/each}
		</div>
		<div class="agent-inputbox">
			<textarea
				bind:this={textarea}
				bind:value
				rows="1"
				{placeholder}
				aria-label="Ask the assistant"
				disabled={busy}
				on:input={grow}
				on:keydown={keydown}
			></textarea>
			<div class="agent-input-foot">
				<div class="agent-modes" role="group" aria-label="Assistant mode">
					<button
						class="agent-mode"
						class:active={mode === 'auto'}
						title="Let the assistant decide whether to answer or draft"
						on:click={() => onMode('auto')}
						disabled={busy}
					>
						Auto
					</button>
					<button
						class="agent-mode"
						class:active={mode === 'ask'}
						title="Answer questions; do not propose edits"
						on:click={() => onMode('ask')}
						disabled={busy}
					>
						Ask
					</button>
					<button
						class="agent-mode"
						class:active={mode === 'edit'}
						title="Expect a concrete draft"
						on:click={() => onMode('edit')}
						disabled={busy}
					>
						Edit
					</button>
				</div>
				{#if hasSelection}
					<span class="agent-context-chip">Selection included</span>
				{/if}
				<div class="spacer"></div>
				{#if busy}
					<button class="agent-send stop" on:click={onStop} title="Stop" aria-label="Stop">
						■
					</button>
				{:else}
					<button
						class="agent-send"
						on:click={onSend}
						disabled={!value.trim() || disabled}
						title="Send"
						aria-label="Send"
					>
						↑
					</button>
				{/if}
			</div>
		</div>
		<div class="agent-composer-hint">
			<span><span class="kbd">Enter</span> send · <span class="kbd">Shift</span>+<span class="kbd">Enter</span> newline</span>
		</div>
	{/if}
</div>
