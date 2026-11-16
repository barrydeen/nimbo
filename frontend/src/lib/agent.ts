/**
 * Frontend types and pure helpers for the vault assistant.
 *
 * The streaming protocol is SSE with typed events. Draft application is
 * deliberately client-side: the backend validates a draft against the buffer
 * it was shown, then the UI re-validates the exact anchors against the live
 * editor and saves through the normal optimistic-concurrency endpoint.
 */

export type AgentMode = 'auto' | 'ask' | 'edit';
export type AgentDraftKind = 'edit' | 'create' | 'append' | 'replace';
export type DraftStatus = 'pending' | 'applying' | 'applied' | 'discarded' | 'error';

export type AgentDraftEdit = {
	find: string;
	replace: string;
	start?: number;
	end?: number;
	start_line?: number;
	end_line?: number;
};

export type AgentDraft = {
	id: string;
	kind: AgentDraftKind;
	path: string;
	title?: string;
	rationale: string;
	base_hash?: string | null;
	base_version?: number;
	edits?: AgentDraftEdit[];
	content?: string;
};

export type AgentToolState = 'start' | 'done' | 'error';

export type AgentToolStep = {
	id: string;
	name: string;
	state: AgentToolState;
	summary: string;
	detail?: string;
};

export type AgentTextPart = { type: 'text'; text: string };
export type AgentToolPart = { type: 'tool'; tool: AgentToolStep };
export type AgentDraftPart = {
	type: 'draft';
	draft: AgentDraft;
	status: DraftStatus;
	selected: boolean[];
	error?: string;
};
export type AgentPart = AgentTextPart | AgentToolPart | AgentDraftPart;

export type AgentMessage = {
	id: number;
	role: 'user' | 'assistant';
	parts: AgentPart[];
	streaming?: boolean;
	stopped?: boolean;
	error?: string;
};

export type AgentSseEvent =
	| { type: 'status'; label: string }
	| { type: 'text'; text: string }
	| { type: 'tool'; tool: AgentToolStep }
	| { type: 'draft'; draft: AgentDraft }
	| { type: 'done'; reason: string }
	| { type: 'error'; message: string };

const MAX_SSE_BUFFER = 2 * 1024 * 1024;

function parseJson<T>(data: string): T | null {
	try {
		return JSON.parse(data) as T;
	} catch {
		return null;
	}
}

/**
 * Incremental SSE frame parser. It keeps partial frames across chunks, joins
 * multiple `data:` lines, ignores comments, and returns typed agent events.
 */
export class AgentSseParser {
	private buffer = '';
	private event = 'message';
	private data: string[] = [];

	push(chunk: string): AgentSseEvent[] {
		this.buffer += chunk;
		this.buffer = this.buffer.replace(/\r\n/g, '\n');
		if (this.buffer.length > MAX_SSE_BUFFER) {
			throw new Error('The assistant response exceeded the stream buffer limit.');
		}
		const events: AgentSseEvent[] = [];
		let cut: number;
		while ((cut = this.buffer.indexOf('\n\n')) !== -1) {
			const frame = this.buffer.slice(0, cut);
			this.buffer = this.buffer.slice(cut + 2);
			const event = this.parseFrame(frame);
			if (event) events.push(event);
		}
		return events;
	}

	finish(): AgentSseEvent[] {
		if (!this.buffer.trim()) {
			this.buffer = '';
			return [];
		}
		const event = this.parseFrame(this.buffer);
		this.buffer = '';
		return event ? [event] : [];
	}

	private parseFrame(frame: string): AgentSseEvent | null {
		this.event = 'message';
		this.data = [];
		for (const line of frame.split('\n')) {
			if (!line || line.startsWith(':')) continue;
			if (line.startsWith('event:')) {
				this.event = line.slice(6).trim() || 'message';
			} else if (line.startsWith('data:')) {
				this.data.push(line.slice(5).replace(/^ /, ''));
			}
		}
		const data = this.data.join('\n');
		if (!data) return null;

		switch (this.event) {
			case 'status': {
				const parsed = parseJson<{ label?: string }>(data);
				return { type: 'status', label: parsed?.label || 'Working…' };
			}
			case 'text':
			case 'message':
				return { type: 'text', text: data };
			case 'tool': {
				const parsed = parseJson<AgentToolStep>(data);
				return parsed ? { type: 'tool', tool: parsed } : null;
			}
			case 'draft': {
				const parsed = parseJson<AgentDraft>(data);
				return parsed ? { type: 'draft', draft: parsed } : null;
			}
			case 'done': {
				const parsed = parseJson<{ reason?: string }>(data);
				return { type: 'done', reason: parsed?.reason || 'stop' };
			}
			case 'error':
				return { type: 'error', message: data };
			default:
				return null;
		}
	}
}

export type DraftChange = { from: number; to: number; insert: string };
export type DraftRange = { from: number; to: number };

export type DraftApplyResult =
	| { ok: true; content: string; changes: DraftChange[]; ranges: DraftRange[] }
	| { ok: false; error: string };

/**
 * Validates exact find/replace anchors against `content` and returns the
 * resulting text plus CodeMirror changes (ascending, non-overlapping).
 */
export function applyDraftEditsToText(
	content: string,
	edits: AgentDraftEdit[]
): DraftApplyResult {
	if (edits.length === 0) {
		return { ok: false, error: 'No changes are selected.' };
	}
	const ranges: { from: number; to: number; insert: string }[] = [];
	for (let index = 0; index < edits.length; index += 1) {
		const edit = edits[index];
		if (!edit.find) {
			return { ok: false, error: `Change ${index + 1} has no anchor text.` };
		}
		const first = content.indexOf(edit.find);
		if (first < 0) {
			return {
				ok: false,
				error: 'The text this draft replaces has changed. Regenerate the draft.'
			};
		}
		if (content.indexOf(edit.find, first + 1) >= 0) {
			return {
				ok: false,
				error: 'The text this draft replaces is no longer unique. Regenerate the draft.'
			};
		}
		ranges.push({ from: first, to: first + edit.find.length, insert: edit.replace });
	}
	ranges.sort((a, b) => a.from - b.from);
	for (let index = 1; index < ranges.length; index += 1) {
		if (ranges[index].from < ranges[index - 1].to) {
			return { ok: false, error: 'The selected changes overlap. Regenerate the draft.' };
		}
	}
	let next = content;
	for (const range of [...ranges].sort((a, b) => b.from - a.from)) {
		next = next.slice(0, range.from) + range.insert + next.slice(range.to);
	}
	let delta = 0;
	const resultRanges = ranges.map((range) => {
		const from = range.from + delta;
		const to = from + range.insert.length;
		delta += range.insert.length - (range.to - range.from);
		return { from, to };
	});
	return {
		ok: true,
		content: next,
		changes: ranges,
		ranges: resultRanges
	};
}

export type DiffLine = {
	kind: 'context' | 'add' | 'del';
	text: string;
	oldLine?: number;
	newLine?: number;
};

/**
 * Small, bounded line diff for draft previews. It falls back to a plain
 * before/after view when a hunk is too large to diff cheaply.
 */
export function lineDiff(before: string, after: string, startLine = 1): DiffLine[] {
	const oldLines = before.split('\n');
	const newLines = after.split('\n');
	if (oldLines.length * newLines.length > 40_000) {
		return [
			...oldLines.map((text, index) => ({
				kind: 'del' as const,
				text,
				oldLine: startLine + index
			})),
			...newLines.map((text, index) => ({
				kind: 'add' as const,
				text,
				newLine: startLine + index
			}))
		];
	}

	const table: number[][] = Array.from({ length: oldLines.length + 1 }, () =>
		new Array<number>(newLines.length + 1).fill(0)
	);
	for (let i = oldLines.length - 1; i >= 0; i -= 1) {
		for (let j = newLines.length - 1; j >= 0; j -= 1) {
			table[i][j] =
				oldLines[i] === newLines[j]
					? table[i + 1][j + 1] + 1
					: Math.max(table[i + 1][j], table[i][j + 1]);
		}
	}

	const out: DiffLine[] = [];
	let i = 0;
	let j = 0;
	while (i < oldLines.length && j < newLines.length) {
		if (oldLines[i] === newLines[j]) {
			out.push({
				kind: 'context',
				text: oldLines[i],
				oldLine: startLine + i,
				newLine: startLine + j
			});
			i += 1;
			j += 1;
		} else if (table[i + 1][j] >= table[i][j + 1]) {
			out.push({ kind: 'del', text: oldLines[i], oldLine: startLine + i });
			i += 1;
		} else {
			out.push({ kind: 'add', text: newLines[j], newLine: startLine + j });
			j += 1;
		}
	}
	while (i < oldLines.length) {
		out.push({ kind: 'del', text: oldLines[i], oldLine: startLine + i });
		i += 1;
	}
	while (j < newLines.length) {
		out.push({ kind: 'add', text: newLines[j], newLine: startLine + j });
		j += 1;
	}
	return out;
}

export function selectedEdits(part: AgentDraftPart): AgentDraftEdit[] {
	return (part.draft.edits ?? []).filter((_, index) => part.selected[index] !== false);
}

const STORAGE_PREFIX = 'nimbo.agent.v1:';
const MAX_STORED_MESSAGES = 40;
const MAX_STORED_DRAFTS = 8;
const MAX_STORED_BYTES = 200 * 1024;

export type StoredConversation = {
	messages: AgentMessage[];
	mode: AgentMode;
};

function storageKey(key: string): string {
	return `${STORAGE_PREFIX}${key}`;
}

export function loadConversation(key: string): StoredConversation | null {
	if (typeof sessionStorage === 'undefined') return null;
	try {
		const raw = sessionStorage.getItem(storageKey(key));
		if (!raw) return null;
		const parsed = JSON.parse(raw) as StoredConversation;
		if (!parsed || !Array.isArray(parsed.messages)) return null;
		const messages = parsed.messages
			.filter((message) => message && (message.role === 'user' || message.role === 'assistant'))
			.slice(-MAX_STORED_MESSAGES)
			.map((message) => ({
				...message,
				streaming: false,
				parts: Array.isArray(message.parts)
					? message.parts
							.filter((part) => part && typeof part === 'object' && 'type' in part)
							.map((part) =>
								part.type === 'draft' && part.status === 'applying'
									? { ...part, status: 'pending' as const }
									: part
							)
					: []
			}));
		const mode =
			parsed.mode === 'ask' || parsed.mode === 'edit' || parsed.mode === 'auto'
				? parsed.mode
				: 'auto';
		return { messages, mode };
	} catch {
		return null;
	}
}

export function saveConversation(key: string, data: StoredConversation): void {
	if (typeof sessionStorage === 'undefined') return;
	try {
		let messages = data.messages.slice(-MAX_STORED_MESSAGES);
		let drafts = 0;
		messages = messages.map((message) => ({
			...message,
			parts: message.parts.filter((part) => {
				if (part.type !== 'draft') return true;
				drafts += 1;
				return drafts <= MAX_STORED_DRAFTS;
			})
		}));
		const serialized = JSON.stringify({ messages, mode: data.mode });
		if (serialized.length > MAX_STORED_BYTES) {
			const slim = messages.slice(-20).map((message) => ({
				...message,
				parts: message.parts.filter((part) => part.type !== 'text' || part.text.length < 4_000)
			}));
			const compact = JSON.stringify({ messages: slim, mode: data.mode });
			if (compact.length <= MAX_STORED_BYTES) {
				sessionStorage.setItem(storageKey(key), compact);
			}
			return;
		}
		sessionStorage.setItem(storageKey(key), serialized);
	} catch {
		// Storage can be disabled or full; losing chat history must not break the editor.
	}
}

export function clearConversation(key: string): void {
	if (typeof sessionStorage === 'undefined') return;
	try {
		sessionStorage.removeItem(storageKey(key));
	} catch {
		// Ignore unavailable storage.
	}
}
