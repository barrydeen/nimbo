#!/usr/bin/env node
/**
 * End-to-end agent smoke test with a deterministic OpenAI-compatible provider.
 *
 * Proves: chat-only responses, tool-call drafting, no write before acceptance,
 * accept/discard, create, replace, append, stale-hash detection, and the
 * no-tools fallback protocol.
 */
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

const root = path.resolve(import.meta.dirname, '..');
const backendDir = path.join(root, 'backend');
const binary = path.join(backendDir, 'target/debug/nimbo');
const dataDir = mkdtempSync(path.join(tmpdir(), 'nimbo-agent-smoke-'));

const originalContent = '# Test\n\nAlpha\nBeta\nGamma\n';

function freePort() {
	return new Promise((resolve, reject) => {
		const server = createServer();
		server.listen(0, '127.0.0.1', () => {
			const address = server.address();
			server.close(() => resolve(address.port));
		});
		server.on('error', reject);
	});
}

function readJson(req) {
	return new Promise((resolve, reject) => {
		let body = '';
		req.setEncoding('utf8');
		req.on('data', (chunk) => {
			body += chunk;
			if (body.length > 2 * 1024 * 1024) {
				reject(new Error('mock request too large'));
				req.destroy();
			}
		});
		req.on('end', () => {
			try {
				resolve(JSON.parse(body || '{}'));
			} catch (err) {
				reject(err);
			}
		});
		req.on('error', reject);
	});
}

function sse(res, frames) {
	res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
	for (const frame of frames) res.write(`data: ${JSON.stringify(frame)}\n\n`);
	res.write('data: [DONE]\n\n');
	res.end();
}

function streamText(res, text) {
	const chunks = text.match(/[\s\S]{1,24}/g) ?? [text];
	const frames = chunks.map((chunk) => ({ choices: [{ delta: { content: chunk } }] }));
	frames.push({ choices: [{ delta: {}, finish_reason: 'stop' }] });
	sse(res, frames);
}

function streamTool(res, name, args, id = `call-${name}-${Date.now()}`) {
	sse(res, [
		{
			choices: [
				{
					delta: {
						tool_calls: [
							{
								index: 0,
								id,
								type: 'function',
								function: { name, arguments: JSON.stringify(args) }
							}
						]
					},
					finish_reason: 'tool_calls'
				}
			]
		}
	]);
}

async function main() {
	const providerRequests = [];
	const provider = createServer(async (req, res) => {
		try {
			if (req.method !== 'POST' || req.url !== '/v1/chat/completions') {
				res.writeHead(404, { 'content-type': 'application/json' });
				res.end('{"error":"not found"}');
				return;
			}
			const body = await readJson(req);
			providerRequests.push(body);
			const messages = Array.isArray(body.messages) ? body.messages : [];
			const lastUser = [...messages].reverse().find((message) => message.role === 'user')?.content ?? '';
			const tools = Array.isArray(body.tools) ? body.tools : null;
			const lastAssistant = [...messages]
				.reverse()
				.find((message) => message.role === 'assistant' && Array.isArray(message.tool_calls));
			const lastToolName = lastAssistant?.tool_calls?.[0]?.function?.name;

			if (lastUser.includes('FALLBACK_DRAFT') && tools) {
				res.writeHead(400, { 'content-type': 'application/json' });
				res.end('{"error":"tools are not supported by this provider"}');
				return;
			}
			if (lastToolName) {
				if (lastToolName === 'search_documents') {
					streamTool(res, 'propose_edit', {
						path: 'notes/test.md',
						rationale: 'Update beta.',
						edits: [{ find: 'Beta', replace: 'Delta' }]
					});
					return;
				}
				streamText(res, "I've drafted a change. Review it in the sidebar.");
				return;
			}
			if (lastUser.includes('EDIT_DRAFT')) {
				streamTool(res, 'search_documents', { query: 'Alpha' });
				return;
			}
			if (lastUser.includes('CREATE_DRAFT')) {
				streamTool(res, 'create_document', {
					path: 'notes/new.md',
					content: '# New\n\nBody.\n',
					rationale: 'Create a note.'
				});
				return;
			}
			if (lastUser.includes('REPLACE_DRAFT')) {
				streamTool(res, 'replace_document', {
					path: 'notes/test.md',
					content: '# Test\n\nReplaced.\n',
					rationale: 'Replace the note.'
				});
				return;
			}
			if (lastUser.includes('APPEND_DRAFT')) {
				streamTool(res, 'append_to_document', {
					path: 'notes/test.md',
					content: 'Appended.\n',
					rationale: 'Append a line.'
				});
				return;
			}
			if (lastUser.includes('FALLBACK_DRAFT')) {
				streamText(
					res,
					'Here is a draft.\n\n```nimbo-draft\n' +
						JSON.stringify({
							kind: 'edit',
							path: 'notes/test.md',
							rationale: 'Fallback edit.',
							edits: [{ find: 'Beta', replace: 'Delta' }]
						}) +
						'\n```\n'
				);
				return;
			}
			streamText(res, 'Hello from the mock provider.');
		} catch (err) {
			res.writeHead(500, { 'content-type': 'application/json' });
			res.end(JSON.stringify({ error: String(err) }));
		}
	});
	await new Promise((resolve, reject) => {
		provider.listen(0, '127.0.0.1', resolve);
		provider.on('error', reject);
	});
	const providerPort = provider.address().port;
	const backendPort = await freePort();
	const base = `http://127.0.0.1:${backendPort}`;

	execFileSync('cargo', ['build', '--quiet'], { cwd: backendDir, stdio: 'inherit' });
	const backend = spawn(binary, [], {
		env: {
			...process.env,
			NIMBO_DATA_DIR: path.join(dataDir, 'data'),
			NIMBO_BIND: `127.0.0.1:${backendPort}`,
			RUST_LOG: 'nimbo=warn'
		},
		stdio: ['ignore', 'pipe', 'pipe']
	});
	let backendLogs = '';
	backend.stdout.on('data', (chunk) => (backendLogs += chunk.toString()));
	backend.stderr.on('data', (chunk) => (backendLogs += chunk.toString()));

	let cookie = '';
	async function request(urlPath, options = {}) {
		const headers = { ...(options.headers ?? {}) };
		if (cookie) headers.cookie = cookie;
		const response = await fetch(`${base}${urlPath}`, { ...options, headers });
		const setCookies =
			typeof response.headers.getSetCookie === 'function'
				? response.headers.getSetCookie()
				: response.headers.get('set-cookie')
					? [response.headers.get('set-cookie')]
					: [];
		if (setCookies.length) {
			cookie = setCookies.map((value) => value.split(';')[0]).join('; ');
		}
		const text = await response.text();
		let json = null;
		try {
			json = JSON.parse(text);
		} catch {
			json = null;
		}
		return { response, text, json };
	}

	function parseSse(text) {
		const events = [];
		for (const frame of text.split('\n\n')) {
			if (!frame.trim()) continue;
			let event = 'message';
			const data = [];
			for (const line of frame.split('\n')) {
				if (line.startsWith('event:')) event = line.slice(6).trim();
				else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
			}
			if (!data.length) continue;
			const payload = data.join('\n');
			let json = null;
			try {
				json = JSON.parse(payload);
			} catch {
				json = null;
			}
			events.push({ event, payload, json });
		}
		return events;
	}

	async function chat(message, content, hash, mode = 'auto') {
		const response = await request('/api/agent/chat', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				message,
				path: 'notes/test.md',
				doc: { content, hash, dirty: false, version: 1 },
				mode
			})
		});
		assert.equal(response.response.status, 200, response.text);
		return parseSse(response.text);
	}

	async function getDoc(pathValue = 'notes/test.md') {
		const response = await request(`/api/docs/content?path=${encodeURIComponent(pathValue)}`);
		return { status: response.response.status, json: response.json };
	}

	async function putDoc(content, baseHash) {
		return request('/api/docs/content', {
			method: 'PUT',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ path: 'notes/test.md', content, base_hash: baseHash })
		});
	}

	try {
		let ready = false;
		for (let attempt = 0; attempt < 120; attempt += 1) {
			try {
				const response = await fetch(`${base}/healthz`);
				if (response.ok) {
					ready = true;
					break;
				}
			} catch {
				// Server is still starting.
			}
			await sleep(250);
		}
		assert.ok(ready, `backend did not become healthy\n${backendLogs}`);

		const setup = await request('/api/setup', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				instance_name: 'Agent Smoke',
				username: 'smokeadmin',
				password: 'correct horse battery staple'
			})
		});
		assert.ok([200, 201].includes(setup.response.status), setup.text);
		assert.ok(cookie, 'setup did not issue a session cookie');

		const settings = await request('/api/settings/llm', {
			method: 'PUT',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				providers: [
					{
						id: 'mock',
						name: 'Mock provider',
						base_url: `http://127.0.0.1:${providerPort}/v1`,
						models: ['mock-model']
					}
				],
				document_model: { provider_id: 'mock', model: 'mock-model' },
				temperature: 0.1
			})
		});
		assert.equal(settings.response.status, 200, settings.text);

		const created = await putDoc(originalContent, null);
		assert.equal(created.response.status, 200, created.text);
		let doc = await getDoc();
		assert.equal(doc.status, 200);
		assert.equal(doc.json.content, originalContent);

		// Chat-only: no draft, no write.
		const chatEvents = await chat('CHAT_ONLY', doc.json.content, doc.json.hash);
		const chatText = chatEvents
			.filter((event) => event.event === 'text')
			.map((event) => event.payload)
			.join('');
		assert.match(chatText, /Hello from the mock provider/);
		assert.equal(chatEvents.some((event) => event.event === 'draft'), false);

		// Edit draft: tool calls, a draft, and no disk write.
		const editEvents = await chat('EDIT_DRAFT', doc.json.content, doc.json.hash);
		const editDraft = editEvents.find(
			(event) => event.event === 'draft' && event.json?.kind === 'edit'
		)?.json;
		assert.ok(editDraft, JSON.stringify(editEvents));
		assert.equal(editDraft.path, 'notes/test.md');
		assert.equal(editDraft.edits[0].find, 'Beta');
		assert.equal(editDraft.edits[0].replace, 'Delta');
		assert.ok(editDraft.base_hash);
		doc = await getDoc();
		assert.equal(doc.json.content, originalContent, 'draft must not write to disk');
		assert.ok(editEvents.some((event) => event.event === 'tool' && event.json?.state === 'done'));

		// Discard: the document is still unchanged.
		doc = await getDoc();
		assert.equal(doc.json.content, originalContent);

		// Stale: an external change changes the hash used by the draft.
		const oldHash = editDraft.base_hash;
		const changed = await putDoc(`${originalContent}\nExternal change.\n`, doc.json.hash);
		assert.equal(changed.response.status, 200, changed.text);
		doc = await getDoc();
		assert.notEqual(doc.json.hash, oldHash);
		assert.equal(
			editDraft.edits[0].find,
			'Beta',
			'the draft anchors remain available for client-side revalidation'
		);
		const reset = await putDoc(originalContent, doc.json.hash);
		assert.equal(reset.response.status, 200, reset.text);
		doc = await getDoc();

		// Accept the draft with the normal optimistic-concurrency save.
		const accepted = await putDoc(
			doc.json.content.replace('Beta', 'Delta'),
			doc.json.hash
		);
		assert.equal(accepted.response.status, 200, accepted.text);
		doc = await getDoc();
		assert.match(doc.json.content, /Delta/);
		assert.doesNotMatch(doc.json.content, /Beta/);

		// Create draft: no file until acceptance, then a new document exists.
		const createEvents = await chat('CREATE_DRAFT', doc.json.content, doc.json.hash);
		const createDraft = createEvents.find(
			(event) => event.event === 'draft' && event.json?.kind === 'create'
		)?.json;
		assert.ok(createDraft, JSON.stringify(createEvents));
		assert.equal(createDraft.path, 'notes/new.md');
		assert.equal((await getDoc('notes/new.md')).status, 404);
		const createAccept = await request('/api/docs/content', {
			method: 'PUT',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				path: createDraft.path,
				content: createDraft.content,
				base_hash: null
			})
		});
		assert.equal(createAccept.response.status, 200, createAccept.text);
		assert.equal((await getDoc('notes/new.md')).status, 200);

		// Replace draft.
		doc = await getDoc();
		const replaceEvents = await chat('REPLACE_DRAFT', doc.json.content, doc.json.hash);
		const replaceDraft = replaceEvents.find(
			(event) => event.event === 'draft' && event.json?.kind === 'replace'
		)?.json;
		assert.ok(replaceDraft, JSON.stringify(replaceEvents));
		assert.match(replaceDraft.content, /Replaced/);
		const replaceAccept = await putDoc(replaceDraft.content, doc.json.hash);
		assert.equal(replaceAccept.response.status, 200, replaceAccept.text);
		assert.match((await getDoc()).json.content, /Replaced/);

		// Append draft.
		doc = await getDoc();
		const appendEvents = await chat('APPEND_DRAFT', doc.json.content, doc.json.hash);
		const appendDraft = appendEvents.find(
			(event) => event.event === 'draft' && event.json?.kind === 'append'
		)?.json;
		assert.ok(appendDraft, JSON.stringify(appendEvents));
		assert.match(appendDraft.content, /Appended/);
		const appendAccept = await putDoc(doc.json.content + appendDraft.content, doc.json.hash);
		assert.equal(appendAccept.response.status, 200, appendAccept.text);
		assert.match((await getDoc()).json.content, /Appended/);

		// Reset for the fallback test.
		doc = await getDoc();
		const resetAgain = await putDoc(originalContent, doc.json.hash);
		assert.equal(resetAgain.response.status, 200, resetAgain.text);
		doc = await getDoc();
		const beforeFallbackRequests = providerRequests.length;
		const fallbackEvents = await chat('FALLBACK_DRAFT', doc.json.content, doc.json.hash);
		const fallbackDraft = fallbackEvents.find(
			(event) => event.event === 'draft' && event.json?.kind === 'edit'
		)?.json;
		assert.ok(fallbackDraft, JSON.stringify(fallbackEvents));
		assert.equal(fallbackDraft.edits[0].find, 'Beta');
		const fallbackRequests = providerRequests.slice(beforeFallbackRequests);
		assert.equal(fallbackRequests.length, 2, 'fallback should retry once without tools');
		assert.ok(fallbackRequests[0].tools, 'first fallback request should offer tools');
		assert.equal(fallbackRequests[1].tools, undefined, 'retry must omit tools');
		assert.equal((await getDoc()).json.content, originalContent);

		// The mock provider sees the authoritative buffer, including unsaved edits.
		await chat('CHAT_ONLY', `${originalContent}\nUnsaved marker.\n`, doc.json.hash);
		assert.ok(
			providerRequests.some((body) => JSON.stringify(body).includes('Unsaved marker')),
			'context should include the current editor buffer'
		);

		console.log('agent smoke passed');
	} finally {
		backend.kill('SIGTERM');
		await sleep(300);
		if (!backend.killed) backend.kill('SIGKILL');
		await new Promise((resolve) => provider.close(resolve));
		rmSync(dataDir, { recursive: true, force: true });
	}
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
