<script lang="ts">
	import { onMount } from 'svelte';
	import { EditorView, keymap } from '@codemirror/view';
	import { EditorState } from '@codemirror/state';
	import { basicSetup } from 'codemirror';
	import { markdown } from '@codemirror/lang-markdown';
	import { indentWithTab } from '@codemirror/commands';
	import { autocompletion, type CompletionContext } from '@codemirror/autocomplete';
	import { oneDark } from '@codemirror/theme-one-dark';

	export let value = '';
	export let docPaths: string[] = [];
	export let onchange: (next: string) => void = () => {};
	export let onsave: () => void = () => {};

	let host: HTMLDivElement;
	let view: EditorView | null = null;

	function wikiSource(context: CompletionContext) {
		const before = context.matchBefore(/\[\[([^\]\n]*)$/);
		if (!before) return null;
		const typed = before.text.slice(2).toLowerCase();
		const options = docPaths
			.filter((path) => path.toLowerCase().includes(typed))
			.slice(0, 20)
			.map((path) => ({ label: path, type: 'file' }));
		if (options.length === 0) return null;
		return {
			from: before.from + 2,
			options,
			validFor: /^[^\]\n]*$/
		};
	}

	onMount(() => {
		view = new EditorView({
			state: EditorState.create({
				doc: value,
				extensions: [
					basicSetup,
					markdown(),
					oneDark,
					EditorView.lineWrapping,
					autocompletion({ override: [wikiSource] }),
					keymap.of([
						{
							key: 'Mod-s',
							run: () => {
								onsave();
								return true;
							}
						},
						indentWithTab
					]),
					EditorView.updateListener.of((update) => {
						if (update.docChanged) {
							onchange(update.state.doc.toString());
						}
					}),
					EditorView.theme({
						'&': {
							backgroundColor: 'transparent',
							color: '#e7eaf1',
							height: '100%'
						},
						'.cm-content': { caretColor: '#8ab4ff' },
						'.cm-gutters': {
							backgroundColor: 'transparent',
							border: 'none',
							color: '#4b5363'
						},
						'.cm-activeLine': { backgroundColor: 'rgba(255,255,255,0.03)' },
						'.cm-activeLineGutter': { backgroundColor: 'transparent' },
						'&.cm-focused': { outline: 'none' },
						'.cm-scroller': {
							fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
							fontSize: '14px',
							lineHeight: '1.7'
						},
						'.cm-selectionBackground, ::selection': {
							backgroundColor: 'rgba(138,180,255,0.25) !important'
						}
					})
				]
			}),
			parent: host
		});

		return () => view?.destroy();
	});
</script>

<div class="editor-host" bind:this={host}></div>
