<script lang="ts">
	import { onMount } from 'svelte';
	import { EditorView, keymap, Decoration, WidgetType } from '@codemirror/view';
	import { EditorState, StateEffect, StateField } from '@codemirror/state';
	import { basicSetup } from 'codemirror';
	import { markdown } from '@codemirror/lang-markdown';
	import { indentWithTab } from '@codemirror/commands';
	import { autocompletion, completionStatus, type CompletionContext } from '@codemirror/autocomplete';
	import { oneDark } from '@codemirror/theme-one-dark';
	import { api } from '$lib/api';

	export let value = '';
	export let docPaths: string[] = [];
	export let aiEnabled = false;
	export let onchange: (next: string) => void = () => {};
	export let onsave: () => void = () => {};

	type Ghost = { from: number; text: string } | null;
	type AgentResult = {
		action: string;
		text: string;
		from: number;
		to: number;
		original: string;
	};

	const setGhost = StateEffect.define<Ghost>();

	const ghostField = StateField.define<Ghost>({
		create: () => null,
		update(value, tr) {
			for (const effect of tr.effects) {
				if (effect.is(setGhost)) return effect.value;
			}
			if (tr.docChanged) return null;
			if (tr.selection) return null;
			return value;
		}
	});

	class GhostWidget extends WidgetType {
		text: string;
		constructor(text: string) {
			super();
			this.text = text;
		}
		eq(other: GhostWidget) {
			return other.text === this.text;
		}
		toDOM() {
			const span = document.createElement('span');
			span.className = 'cm-ghost';
			span.textContent = this.text;
			span.setAttribute('aria-hidden', 'true');
			return span;
		}
	}

	const ghostDecorations = EditorView.decorations.compute([ghostField], (state) => {
		const ghost = state.field(ghostField);
		if (!ghost) return Decoration.none;
		return Decoration.set([
			Decoration.widget({ widget: new GhostWidget(ghost.text), side: 1 }).range(ghost.from)
		]);
	});

	let host: HTMLDivElement;
	let view: EditorView | null = null;

	let suggestTimer: ReturnType<typeof setTimeout> | null = null;
	let suggestBusy = false;
	let suggestPending = false;
	let suggestSeq = 0;

	let agentBusy = '';
	let agentError = '';
	let agentResult: AgentResult | null = null;

	function message(err: unknown): string {
		return err instanceof Error ? err.message : 'Something went wrong';
	}

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

	function clearSuggestTimer() {
		if (suggestTimer) {
			clearTimeout(suggestTimer);
			suggestTimer = null;
		}
	}

	function scheduleSuggest(delay = 800) {
		if (!aiEnabled || !view) return;
		if (suggestBusy) {
			suggestPending = true;
			return;
		}
		clearSuggestTimer();
		suggestTimer = setTimeout(() => {
			suggestTimer = null;
			void requestSuggestion();
		}, delay);
	}

	function cleanSuggestion(raw: string): string {
		let text = raw.trim();
		text = text.replace(/^```[^\n]*\n?/, '').replace(/\n?```$/, '').trim();
		text = text.replace(/^["'`]+|["'`]+$/g, '').trim();
		return text.split('\n').map((line) => line.trim()).find((line) => line.length > 0) ?? '';
	}

	function prepareSuggestion(prefix: string, raw: string): string {
		const text = cleanSuggestion(raw);
		if (!text) return '';
		const before = prefix.slice(-1);
		const atLineStart = prefix.length === 0 || before === '\n';
		const currentLine = prefix.slice(prefix.lastIndexOf('\n') + 1);
		const isBlockStart = /^(#{1,6}\s|[-*+]\s|\d+\.\s|>\s|```|\|)/.test(text);
		if (!atLineStart && currentLine.trim().length > 0 && isBlockStart) {
			return `\n\n${text}`;
		}
		if (!atLineStart && before && !/\s/.test(before) && !/^[.,;:!?)\]}]/.test(text)) {
			return ` ${text}`;
		}
		return text;
	}

	async function requestSuggestion() {
		const current = view;
		if (!current || !aiEnabled || suggestBusy || !current.hasFocus) return;
		const selection = current.state.selection.main;
		if (!selection.empty) return;
		const cursor = selection.head;
		const prefix = current.state.sliceDoc(0, cursor);
		if (!prefix.trim()) return;

		const seq = ++suggestSeq;
		const length = current.state.doc.length;
		suggestBusy = true;
		try {
			const res = await api.agentAction({ action: 'suggest', text: prefix.slice(-4000) });
			if (seq !== suggestSeq || !view || !aiEnabled) return;
			if (view.state.doc.length !== length) return;
			if (view.state.selection.main.head !== cursor) return;
			const text = prepareSuggestion(prefix, res.text);
			if (!text) return;
			agentError = '';
			view.dispatch({ effects: setGhost.of({ from: cursor, text }) });
		} catch (err) {
			if (seq === suggestSeq) agentError = message(err);
		} finally {
			suggestBusy = false;
			if (suggestPending) {
				suggestPending = false;
				scheduleSuggest(400);
			}
		}
	}

	function toggleAi() {
		aiEnabled = !aiEnabled;
		agentError = '';
		if (!view) return;
		view.focus();
		if (aiEnabled) {
			scheduleSuggest(150);
		} else {
			clearSuggestTimer();
			suggestPending = false;
			suggestSeq += 1;
			view.dispatch({ effects: setGhost.of(null) });
		}
	}

	function acceptGhost(target: EditorView): boolean {
		if (completionStatus(target.state) === 'active') return false;
		const ghost = target.state.field(ghostField);
		if (!ghost) return false;
		target.dispatch({
			changes: { from: ghost.from, insert: ghost.text },
			selection: { anchor: ghost.from + ghost.text.length },
			effects: setGhost.of(null),
			userEvent: 'input.complete'
		});
		return true;
	}

	function dismissGhost(target: EditorView): boolean {
		if (!target.state.field(ghostField)) return false;
		target.dispatch({ effects: setGhost.of(null) });
		return true;
	}

	function wrapSelection(before: string, after: string, placeholder: string) {
		const current = view;
		if (!current) return;
		const { from, to } = current.state.selection.main;
		const selected = current.state.sliceDoc(from, to);
		const body = selected || placeholder;
		current.dispatch({
			changes: { from, to, insert: before + body + after },
			selection: { anchor: from + before.length, head: from + before.length + body.length },
			userEvent: 'input.format',
			scrollIntoView: true
		});
		current.focus();
	}

	function insertLink() {
		const current = view;
		if (!current) return;
		const { from, to } = current.state.selection.main;
		const selected = current.state.sliceDoc(from, to);
		const label = selected || 'text';
		const insert = `[${label}](url)`;
		if (selected) {
			const urlStart = from + label.length + 3;
			current.dispatch({
				changes: { from, to, insert },
				selection: { anchor: urlStart, head: urlStart + 3 },
				userEvent: 'input.format'
			});
		} else {
			current.dispatch({
				changes: { from, to, insert },
				selection: { anchor: from + 1, head: from + 1 + label.length },
				userEvent: 'input.format'
			});
		}
		current.focus();
	}

	function setHeading(level: number) {
		const current = view;
		if (!current) return;
		const { from, to } = current.state.selection.main;
		const start = current.state.doc.lineAt(from).number;
		const end = current.state.doc.lineAt(to).number;
		let allMatch = true;
		for (let n = start; n <= end; n++) {
			const match = /^(#{1,6})\s+/.exec(current.state.doc.line(n).text);
			if (!match || match[1].length !== level) {
				allMatch = false;
				break;
			}
		}
		const prefix = `${'#'.repeat(level)} `;
		const changes: { from: number; to?: number; insert: string }[] = [];
		for (let n = start; n <= end; n++) {
			const line = current.state.doc.line(n);
			const match = /^(#{1,6})\s+/.exec(line.text);
			if (allMatch) {
				if (match) changes.push({ from: line.from, to: line.from + match[0].length, insert: '' });
			} else if (match) {
				changes.push({ from: line.from, to: line.from + match[0].length, insert: prefix });
			} else {
				changes.push({ from: line.from, insert: prefix });
			}
		}
		if (changes.length) {
			current.dispatch({ changes, userEvent: 'input.format', scrollIntoView: true });
		}
		current.focus();
	}

	function toggleLinePrefix(prefix: string) {
		const current = view;
		if (!current) return;
		const { from, to } = current.state.selection.main;
		const start = current.state.doc.lineAt(from).number;
		const end = current.state.doc.lineAt(to).number;
		let allHave = true;
		for (let n = start; n <= end; n++) {
			if (!current.state.doc.line(n).text.startsWith(prefix)) {
				allHave = false;
				break;
			}
		}
		const changes: { from: number; to?: number; insert: string }[] = [];
		for (let n = start; n <= end; n++) {
			const line = current.state.doc.line(n);
			if (allHave) {
				changes.push({ from: line.from, to: line.from + prefix.length, insert: '' });
			} else if (!line.text.startsWith(prefix)) {
				changes.push({ from: line.from, insert: prefix });
			}
		}
		if (changes.length) {
			current.dispatch({ changes, userEvent: 'input.format', scrollIntoView: true });
		}
		current.focus();
	}

	async function runAgent(action: 'grammar' | 'rewrite' | 'links' | 'tags') {
		const current = view;
		if (!current || agentBusy) return;
		const selection = current.state.selection.main;
		const useSelection = !selection.empty && (action === 'grammar' || action === 'rewrite');
		const from = useSelection ? selection.from : 0;
		const to = useSelection ? selection.to : current.state.doc.length;
		const text = current.state.sliceDoc(from, to);
		if (!text.trim()) {
			agentError = 'Nothing to send — the document is empty.';
			return;
		}

		agentBusy = action;
		agentError = '';
		agentResult = null;
		try {
			const res = await api.agentAction({ action, text });
			if (!view) return;
			agentResult = { action, text: res.text.trim(), from, to, original: text };
		} catch (err) {
			agentError = message(err);
		} finally {
			agentBusy = '';
		}
	}

	function applyResult() {
		const current = view;
		const result = agentResult;
		if (!current || !result) return;
		agentResult = null;

		if (result.action === 'grammar' || result.action === 'rewrite') {
			const stillThere = current.state.sliceDoc(result.from, result.to) === result.original;
			if (stillThere) {
				current.dispatch({
					changes: { from: result.from, to: result.to, insert: result.text },
					selection: { anchor: result.from + result.text.length },
					userEvent: 'input.agent',
					scrollIntoView: true
				});
				current.focus();
				return;
			}
		}

		const atEnd = result.action === 'links' || result.action === 'tags';
		const pos = atEnd ? current.state.doc.length : current.state.selection.main.head;
		const doc = current.state.doc.toString();
		const needsBreak = pos > 0 && doc[pos - 1] !== '\n';
		const insert = `${needsBreak ? '\n\n' : ''}${result.text}\n`;
		current.dispatch({
			changes: { from: pos, insert },
			selection: { anchor: pos + insert.length },
			userEvent: 'input.agent',
			scrollIntoView: true
		});
		current.focus();
	}

	onMount(() => {
		const editor = new EditorView({
			state: EditorState.create({
				doc: value,
				extensions: [
					basicSetup,
					markdown(),
					oneDark,
					EditorView.lineWrapping,
					ghostField,
					ghostDecorations,
					autocompletion({ override: [wikiSource] }),
					keymap.of([
						{ key: 'Tab', run: acceptGhost },
						{ key: 'Escape', run: dismissGhost },
						{
							key: 'Mod-b',
							run: () => {
								wrapSelection('**', '**', 'bold');
								return true;
							}
						},
						{
							key: 'Mod-i',
							run: () => {
								wrapSelection('*', '*', 'italic');
								return true;
							}
						},
						{
							key: 'Mod-k',
							run: () => {
								insertLink();
								return true;
							}
						},
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
						if (update.docChanged || update.selectionSet) {
							scheduleSuggest();
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
		view = editor;
		if (aiEnabled) scheduleSuggest(250);

		return () => {
			clearSuggestTimer();
			suggestSeq += 1;
			view = null;
			editor.destroy();
		};
	});
</script>

<div class="editor-shell">
	<div class="editor-toolbar">
		<div class="tb-group">
			<button class="tb-btn" title="Heading 1" on:click={() => setHeading(1)}>H1</button>
			<button class="tb-btn" title="Heading 2" on:click={() => setHeading(2)}>H2</button>
			<button class="tb-btn" title="Heading 3" on:click={() => setHeading(3)}>H3</button>
		</div>
		<div class="tb-sep"></div>
		<div class="tb-group">
			<button
				class="tb-btn"
				title="Bold (Ctrl+B)"
				on:click={() => wrapSelection('**', '**', 'bold')}
			>
				<strong>B</strong>
			</button>
			<button
				class="tb-btn"
				title="Italic (Ctrl+I)"
				on:click={() => wrapSelection('*', '*', 'italic')}
			>
				<em>I</em>
			</button>
			<button class="tb-btn" title="Inline code" on:click={() => wrapSelection('`', '`', 'code')}>
				Code
			</button>
		</div>
		<div class="tb-sep"></div>
		<div class="tb-group">
			<button class="tb-btn" title="Link (Ctrl+K)" on:click={insertLink}>Link</button>
			<button class="tb-btn" title="Wikilink" on:click={() => wrapSelection('[[', ']]', 'note')}>
				[[ ]]
			</button>
		</div>
		<div class="tb-sep"></div>
		<div class="tb-group">
			<button class="tb-btn" title="Bullet list" on:click={() => toggleLinePrefix('- ')}>• List</button>
			<button class="tb-btn" title="Numbered list" on:click={() => toggleLinePrefix('1. ')}>
				1. List
			</button>
			<button class="tb-btn" title="Quote" on:click={() => toggleLinePrefix('> ')}>❝</button>
		</div>
		<div class="spacer"></div>
		<div class="tb-group ai">
			<button
				class="tb-btn toggle"
				class:active={aiEnabled}
				aria-pressed={aiEnabled}
				title="Suggest the next line. Press Tab to accept, Esc to dismiss."
				on:click={toggleAi}
			>
				✨ {aiEnabled ? 'Suggesting' : 'Suggest'}
			</button>
			<button class="tb-btn" disabled={!!agentBusy} on:click={() => runAgent('grammar')}>
				Grammar
			</button>
			<button class="tb-btn" disabled={!!agentBusy} on:click={() => runAgent('rewrite')}>
				Rewrite
			</button>
			<button class="tb-btn" disabled={!!agentBusy} on:click={() => runAgent('links')}>
				Links
			</button>
			<button class="tb-btn" disabled={!!agentBusy} on:click={() => runAgent('tags')}>
				Tags
			</button>
		</div>
	</div>

	{#if aiEnabled}
		<div class="editor-hint">
			Inline AI is on. Press <span class="kbd">Tab</span> to accept a suggestion or
			<span class="kbd">Esc</span> to dismiss it.
		</div>
	{/if}

	{#if agentResult}
		<div class="agent-result">
			<div class="agent-result-head">
				<span>
					{agentResult.action === 'grammar'
						? 'Grammar'
						: agentResult.action === 'rewrite'
							? 'Rewrite'
							: agentResult.action === 'links'
								? 'Link suggestions'
								: 'Tag suggestions'}
				</span>
				<div class="spacer"></div>
				<button class="chip apply" on:click={applyResult}>Apply</button>
				<button class="chip" on:click={() => (agentResult = null)}>Discard</button>
			</div>
			<pre>{agentResult.text}</pre>
		</div>
	{/if}

	{#if agentError}
		<div class="editor-error">{agentError}</div>
	{/if}

	<div class="editor-host" bind:this={host}></div>
</div>
