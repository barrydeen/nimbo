<script lang="ts">
	import { onDestroy } from 'svelte';
	import { renderMarkdown } from '$lib/markdown';

	export let text = '';
	export let streaming = false;
	export let onopendoc: (path: string) => void = () => {};

	let html = '';
	let timer: ReturnType<typeof setTimeout> | null = null;
	let lastText = '';

	function render() {
		if (timer) {
			clearTimeout(timer);
			timer = null;
		}
		timer = setTimeout(
			() => {
				timer = null;
				html = renderMarkdown(text);
			},
			streaming ? 90 : 0
		);
	}

	$: if (text !== lastText) {
		lastText = text;
		render();
	}

	function click(event: MouseEvent) {
		const anchor = (event.target as HTMLElement | null)?.closest('a[data-wiki]') as
			| HTMLAnchorElement
			| null;
		if (!anchor) return;
		event.preventDefault();
		onopendoc(decodeURIComponent(anchor.dataset.wiki || ''));
	}

	onDestroy(() => {
		if (timer) clearTimeout(timer);
	});
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="markdown-body agent-markdown" on:click={click}>
	{@html html}
	{#if streaming}<span class="agent-caret" aria-hidden="true"></span>{/if}
</div>
