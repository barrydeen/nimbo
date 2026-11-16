#!/usr/bin/env node
/**
 * Agent protocol and draft-application controls.
 *
 * Bundles the real TypeScript module with esbuild and asserts the streaming
 * parser, anchor validation, and bounded diff behavior. Run with:
 *   node scripts/check-agent.mjs
 */
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { build } from 'esbuild';

const root = path.resolve(import.meta.dirname, '..');
const entry = `export * from ${JSON.stringify(path.join(root, 'src/lib/agent.ts'))};`;

const output = await build({
	stdin: { contents: entry, resolveDir: root, loader: 'ts' },
	bundle: true,
	format: 'esm',
	platform: 'node',
	target: 'node22',
	write: false,
	logLevel: 'silent'
});

const dir = mkdtempSync(path.join(tmpdir(), 'nimbo-agent-'));
const bundled = path.join(dir, 'agent.mjs');
writeFileSync(bundled, output.outputFiles[0].text, 'utf8');

let mod;
try {
	mod = await import(pathToFileURL(bundled).href);
} finally {
	rmSync(dir, { recursive: true, force: true });
}

const { AgentSseParser, applyDraftEditsToText, lineDiff } = mod;

// ---------------------------------------------------------------- SSE parser

{
	const parser = new AgentSseParser();
	const chunks = [
		'event: status\ndata: {"label":"Think',
		'ing…"}\n\n: keep-alive\n\nevent: text\ndata: Hello\n',
		'data: world\n\nevent: tool\ndata: {"id":"t1","name":"search_documents",',
		'"state":"done","summary":"Found 2"}\n\n',
		'event: draft\ndata: {"id":"d1","kind":"edit","path":"a.md","rationale":"r",',
		'"edits":[{"find":"a","replace":"b"}]}\n\n',
		'event: done\ndata: {"reason":"stop"}\n\n'
	];
	const events = chunks.flatMap((chunk) => parser.push(chunk));
	const status = events.find((event) => event.type === 'status');
	assert.equal(status?.label, 'Thinking…');
	const text = events.filter((event) => event.type === 'text').map((event) => event.text);
	assert.deepEqual(text, ['Hello\nworld']);
	const tool = events.find((event) => event.type === 'tool');
	assert.equal(tool?.tool.id, 't1');
	assert.equal(tool?.tool.summary, 'Found 2');
	const draft = events.find((event) => event.type === 'draft');
	assert.equal(draft?.draft.id, 'd1');
	assert.equal(draft?.draft.edits[0].find, 'a');
	const done = events.find((event) => event.type === 'done');
	assert.equal(done?.reason, 'stop');
	assert.deepEqual(parser.push('\n\n'), []);
}

{
	const parser = new AgentSseParser();
	const events = parser.push('event: error\ndata: provider failed\n\n');
	assert.equal(events[0].type, 'error');
	assert.equal(events[0].message, 'provider failed');
}

// ---------------------------------------------------------- draft application

{
	const content = 'Alpha\nBeta\nGamma\n';
	const result = applyDraftEditsToText(content, [
		{ find: 'Beta', replace: 'Delta' },
		{ find: 'Gamma', replace: 'Omega' }
	]);
	assert.equal(result.ok, true);
	assert.equal(result.content, 'Alpha\nDelta\nOmega\n');
	assert.deepEqual(result.changes, [
		{ from: 6, to: 10, insert: 'Delta' },
		{ from: 11, to: 16, insert: 'Omega' }
	]);
}

{
	const result = applyDraftEditsToText('Alpha', [{ find: 'Missing', replace: 'x' }]);
	assert.equal(result.ok, false);
	assert.match(result.error, /changed/i);
}

{
	const result = applyDraftEditsToText('a a', [{ find: 'a', replace: 'x' }]);
	assert.equal(result.ok, false);
	assert.match(result.error, /unique/i);
}

{
	const result = applyDraftEditsToText('abcdef', [
		{ find: 'abc', replace: 'x' },
		{ find: 'bcd', replace: 'y' }
	]);
	assert.equal(result.ok, false);
	assert.match(result.error, /overlap/i);
}

{
	const result = applyDraftEditsToText('Keep\r\nThis\r\n', [{ find: 'This', replace: 'That' }]);
	assert.equal(result.ok, true);
	assert.equal(result.content, 'Keep\r\nThat\r\n');
}

// ------------------------------------------------------------------- diff

{
	const diff = lineDiff('one\ntwo\nthree', 'one\nTWO\nthree');
	assert.ok(diff.some((line) => line.kind === 'del' && line.text === 'two'));
	assert.ok(diff.some((line) => line.kind === 'add' && line.text === 'TWO'));
	assert.ok(diff.some((line) => line.kind === 'context' && line.text === 'one'));
	assert.ok(diff.every((line) => typeof line.text === 'string'));
}

{
	const before = Array.from({ length: 500 }, (_, index) => `old ${index}`).join('\n');
	const after = Array.from({ length: 500 }, (_, index) => `new ${index}`).join('\n');
	const diff = lineDiff(before, after);
	assert.ok(diff.length > 0);
	assert.ok(diff.every((line) => line.kind === 'del' || line.kind === 'add'));
}

console.log('agent protocol/draft controls passed');
