#!/usr/bin/env node
/**
 * Headless Chromium smoke test for the vault assistant panel.
 *
 * Requires the frontend production build (frontend/build) and Chromium. This is
 * intentionally a developer smoke test, not part of the fast CI gate.
 */
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

const root = path.resolve(import.meta.dirname, '..');
const backendDir = path.join(root, 'backend');
const binary = path.join(backendDir, 'target/debug/nimbo');
const staticDir = path.join(root, 'frontend/build');
const dataDir = mkdtempSync(path.join(tmpdir(), 'nimbo-agent-ui-'));
const chromeDir = mkdtempSync(path.join(tmpdir(), 'nimbo-chrome-'));
const shotPath = '/tmp/nimbo-agent-ui.png';
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
			if (body.length > 2 * 1024 * 1024) reject(new Error('mock request too large'));
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

function streamTool(res, name, args) {
	sse(res, [
		{
			choices: [
				{
					delta: {
						tool_calls: [
							{
								index: 0,
								id: `call-${name}-${Date.now()}`,
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

class Cdp {
	constructor(ws) {
		this.ws = ws;
		this.id = 0;
		this.pending = new Map();
		this.listeners = new Map();
		ws.onmessage = (event) => {
			const message = JSON.parse(event.data);
			if (message.id) {
				const pending = this.pending.get(message.id);
				if (!pending) return;
				this.pending.delete(message.id);
				if (message.error) pending.reject(new Error(message.error.message));
				else pending.resolve(message.result);
				return;
			}
			for (const listener of this.listeners.get(message.method) ?? []) {
				listener(message.params);
			}
		};
	}

	static async connect(url) {
		const ws = new WebSocket(url);
		await new Promise((resolve, reject) => {
			ws.onopen = resolve;
			ws.onerror = reject;
		});
		return new Cdp(ws);
	}

	send(method, params = {}) {
		const id = ++this.id;
		return new Promise((resolve, reject) => {
			this.pending.set(id, { resolve, reject });
			this.ws.send(JSON.stringify({ id, method, params }));
		});
	}

	on(method, listener) {
		const listeners = this.listeners.get(method) ?? [];
		listeners.push(listener);
		this.listeners.set(method, listeners);
	}

	close() {
		this.ws.close();
	}
}

async function main() {
	const provider = createServer(async (req, res) => {
		try {
			if (req.method !== 'POST' || req.url !== '/v1/chat/completions') {
				res.writeHead(404).end();
				return;
			}
			const body = await readJson(req);
			const messages = Array.isArray(body.messages) ? body.messages : [];
			const lastUser =
				[...messages].reverse().find((message) => message.role === 'user')?.content ?? '';
			const lastAssistant = [...messages]
				.reverse()
				.find((message) => message.role === 'assistant' && Array.isArray(message.tool_calls));
			const lastToolName = lastAssistant?.tool_calls?.[0]?.function?.name;
			if (lastToolName === 'search_documents') {
				streamTool(res, 'propose_edit', {
					path: 'notes/test.md',
					rationale: 'Update beta.',
					edits: [{ find: 'Beta', replace: 'Delta' }]
				});
				return;
			}
			if (lastToolName) {
				streamText(res, "I've drafted a change. Review it in the sidebar.");
				return;
			}
			if (lastUser.includes('EDIT_DRAFT')) {
				streamTool(res, 'search_documents', { query: 'Alpha' });
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
	const chromePort = await freePort();
	const base = `http://127.0.0.1:${backendPort}`;

	execFileSync('cargo', ['build', '--quiet'], { cwd: backendDir, stdio: 'inherit' });
	const backend = spawn(binary, [], {
		env: {
			...process.env,
			NIMBO_DATA_DIR: path.join(dataDir, 'data'),
			NIMBO_BIND: `127.0.0.1:${backendPort}`,
			NIMBO_STATIC_DIR: staticDir,
			RUST_LOG: 'nimbo=warn'
		},
		stdio: ['ignore', 'pipe', 'pipe']
	});
	let backendLogs = '';
	backend.stdout.on('data', (chunk) => (backendLogs += chunk.toString()));
	backend.stderr.on('data', (chunk) => (backendLogs += chunk.toString()));

	let chrome;
	let cdp;
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
		if (setCookies.length) cookie = setCookies.map((value) => value.split(';')[0]).join('; ');
		const text = await response.text();
		let json = null;
		try {
			json = JSON.parse(text);
		} catch {
			json = null;
		}
		return { response, text, json };
	}

	async function evaluate(expression) {
		const result = await cdp.send('Runtime.evaluate', {
			expression,
			awaitPromise: true,
			returnByValue: true
		});
		if (result.exceptionDetails) {
			throw new Error(result.exceptionDetails.text || 'browser evaluation failed');
		}
		return result.result.value;
	}

	async function waitFor(expression, timeout = 15_000) {
		const started = Date.now();
		while (Date.now() - started < timeout) {
			if (await evaluate(expression)) return true;
			await sleep(120);
		}
		throw new Error(`timed out waiting for ${expression}`);
	}

	async function setInput(value) {
		await evaluate(`(() => {
			const textarea = document.querySelector('.agent-composer textarea');
			textarea.focus();
			textarea.value = ${JSON.stringify(value)};
			textarea.dispatchEvent(new Event('input', { bubbles: true }));
			return true;
		})()`);
		await sleep(80);
	}

	async function send(value, mode) {
		if (mode) {
			await evaluate(`(() => {
				const button = [...document.querySelectorAll('.agent-mode')].find((entry) => entry.textContent.trim() === ${JSON.stringify(mode)});
				button?.click();
				return !!button;
			})()`);
			await sleep(60);
		}
		await setInput(value);
		await evaluate(`document.querySelector('.agent-send').click()`);
	}

	async function setText(value) {
		await setInput(value);
	}

	try {
		let ready = false;
		for (let attempt = 0; attempt < 120; attempt += 1) {
			try {
				if ((await fetch(`${base}/healthz`)).ok) {
					ready = true;
					break;
				}
			} catch {
				// starting
			}
			await sleep(250);
		}
		assert.ok(ready, `backend did not become healthy\n${backendLogs}`);

		const setup = await request('/api/setup', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				instance_name: 'Agent UI',
				username: 'smokeadmin',
				password: 'correct horse battery staple'
			})
		});
		assert.ok([200, 201].includes(setup.response.status), setup.text);
		assert.ok(cookie.startsWith('nimbo_session='), 'missing session cookie');

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

		for (const [docPath, content] of [
			['notes/test.md', originalContent],
			['notes/other.md', '# Other\n\nSeparate note.\n']
		]) {
			const created = await request('/api/docs/content', {
				method: 'PUT',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ path: docPath, content, base_hash: null })
			});
			assert.equal(created.response.status, 200, created.text);
		}

		chrome = spawn(
			'chromium',
			[
				'--headless=new',
				'--no-sandbox',
				'--disable-gpu',
				'--disable-dev-shm-usage',
				`--remote-debugging-port=${chromePort}`,
				`--user-data-dir=${chromeDir}`,
				'about:blank'
			],
			{ stdio: ['ignore', 'ignore', 'ignore'] }
		);

		let targets = null;
		for (let attempt = 0; attempt < 120; attempt += 1) {
			try {
				const response = await fetch(`http://127.0.0.1:${chromePort}/json/list`);
				if (response.ok) {
					targets = await response.json();
					if (targets.some((target) => target.type === 'page')) break;
				}
			} catch {
				// starting
			}
			await sleep(200);
		}
		assert.ok(targets, 'Chromium did not expose a page target');
		const page = targets.find((target) => target.type === 'page');
		cdp = await Cdp.connect(page.webSocketDebuggerUrl);
		await cdp.send('Page.enable');
		await cdp.send('Runtime.enable');
		await cdp.send('Network.enable');
		const sessionToken = cookie.split(';')[0].split('=').slice(1).join('=');
		await cdp.send('Network.setCookie', {
			name: 'nimbo_session',
			value: sessionToken,
			domain: '127.0.0.1',
			path: '/',
			httpOnly: true,
			sameSite: 'Strict'
		});
		await cdp.send('Emulation.setDeviceMetricsOverride', {
			width: 1440,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false
		});
		await cdp.send('Page.navigate', {
			url: `${base}/vault?path=${encodeURIComponent('notes/test.md')}`
		});
		await waitFor(`document.readyState === 'complete'`);
		await waitFor(`!!document.querySelector('.agent-panel')`);
		await waitFor(`document.querySelector('.agent-header')?.innerText.includes('Nimbo Assistant')`);
		await waitFor(`document.querySelector('.toolbar .path')?.textContent.includes('notes/test.md')`);

		// Context card exposes the current document and its link neighborhoods.
		await evaluate(
			`[...document.querySelectorAll('.agent-header-actions .btn')].find((button) => button.textContent.trim() === 'Context')?.click()`
		);
		await waitFor(`!!document.querySelector('.agent-context-card')`);
		const contextText = await evaluate(`document.querySelector('.agent-context-card').innerText`);
		assert.match(contextText, /notes\/test\.md/);
		assert.match(contextText, /Outlinks/i);
		assert.match(contextText, /Backlinks/i);
		await evaluate(
			`[...document.querySelectorAll('.agent-header-actions .btn')].find((button) => button.textContent.trim() === 'Context')?.click()`
		);

		// Chat-only response: text, no draft.
		await send('CHAT_ONLY');
		await waitFor(`document.querySelector('.agent-log')?.innerText.includes('Hello from the mock provider')`);
		assert.equal(await evaluate(`!!document.querySelector('.agent-draft')`), false);

		// Edit mode: tool timeline and a reviewable diff.
		await send('EDIT_DRAFT', 'Edit');
		await waitFor(`!!document.querySelector('.agent-draft')`);
		const draftText = await evaluate(`[...document.querySelectorAll('.agent-draft')].at(-1).innerText`);
		assert.match(draftText, /proposed edit/i);
		assert.match(draftText, /Beta/);
		assert.match(draftText, /Delta/);
		const diskBefore = await request('/api/docs/content?path=notes/test.md');
		assert.equal(diskBefore.json.content, originalContent, 'draft must not write before Apply');
		const draftShot = await cdp.send('Page.captureScreenshot', { format: 'png' });
		writeFileSync('/tmp/nimbo-agent-draft.png', Buffer.from(draftShot.data, 'base64'));

		// Discard leaves the document untouched.
		await evaluate(
			`[...document.querySelectorAll('.agent-draft-actions .btn')].find((button) => button.textContent.trim() === 'Discard')?.click()`
		);
		await waitFor(`document.querySelector('.agent-draft')?.classList.contains('discarded')`);
		assert.equal((await request('/api/docs/content?path=notes/test.md')).json.content, originalContent);

		// Apply writes through the normal save endpoint.
		await send('EDIT_DRAFT', 'Edit');
		await waitFor(`!!document.querySelector('.agent-draft:not(.discarded)')`);
		await evaluate(`document.querySelector('.agent-apply')?.click()`);
		await waitFor(
			`[...document.querySelectorAll('.agent-draft')].at(-1)?.classList.contains('applied')`
		);
		const applied = await request('/api/docs/content?path=notes/test.md');
		assert.match(applied.json.content, /Delta/);
		assert.doesNotMatch(applied.json.content, /Beta/);

		// Undo restores the pre-draft content, then re-apply for the remaining flow.
		await evaluate(
			`[...document.querySelectorAll('.agent-draft-actions .btn')].find((button) => button.textContent.trim() === 'Undo')?.click()`
		);
		await waitFor(
			`[...document.querySelectorAll('.agent-draft')].at(-1)?.classList.contains('applied') === false`
		);
		const undone = await request('/api/docs/content?path=notes/test.md');
		assert.match(undone.json.content, /Beta/);
		await evaluate(`document.querySelector('.agent-apply')?.click()`);
		await waitFor(
			`[...document.querySelectorAll('.agent-draft')].at(-1)?.classList.contains('applied')`
		);

		// Switch documents: each document has its own conversation.
		await evaluate(
			`[...document.querySelectorAll('.doc-item')].find((button) => button.textContent.includes('notes/other.md'))?.click()`
		);
		await waitFor(`document.querySelector('.toolbar .path')?.textContent.includes('notes/other.md')`);
		try {
			await waitFor(
				`document.querySelector('.agent-log')?.innerText.toLowerCase().includes('ask about this note')`
			);
		} catch (err) {
			console.error(
				'agent panel after document switch:\n' +
					(await evaluate(`document.querySelector('.agent-panel')?.innerText`))
			);
			throw err;
		}
		assert.equal(await evaluate(`document.querySelectorAll('.agent-msg').length`), 0);
		await evaluate(
			`[...document.querySelectorAll('.doc-item')].find((button) => button.textContent.includes('notes/test.md'))?.click()`
		);
		await waitFor(`document.querySelector('.toolbar .path')?.textContent.includes('notes/test.md')`);
		await waitFor(`document.querySelector('.agent-log')?.innerText.includes('Hello from the mock provider')`);

		// Narrow viewport: the assistant becomes an openable drawer.
		await cdp.send('Emulation.setDeviceMetricsOverride', {
			width: 900,
			height: 800,
			deviceScaleFactor: 1,
			mobile: false
		});
		await sleep(250);
		assert.equal(await evaluate(`!!document.querySelector('.assistant-toggle')`), true);
		assert.equal(await evaluate(`document.querySelector('.agent-panel')?.classList.contains('open')`), false);
		await evaluate(`document.querySelector('.assistant-toggle')?.click()`);
		await waitFor(`document.querySelector('.agent-panel')?.classList.contains('open')`);
		await evaluate(`document.querySelector('.agent-close')?.click()`);
		await waitFor(`!document.querySelector('.agent-panel')?.classList.contains('open')`);

		await cdp.send('Emulation.setDeviceMetricsOverride', {
			width: 1440,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false
		});
		await sleep(250);
		const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png' });
		writeFileSync(shotPath, Buffer.from(screenshot.data, 'base64'));
		console.log(`agent UI smoke passed; screenshots ${shotPath} and /tmp/nimbo-agent-draft.png`);
	} finally {
		cdp?.close();
		chrome?.kill('SIGTERM');
		backend.kill('SIGTERM');
		await sleep(300);
		backend.kill('SIGKILL');
		await new Promise((resolve) => provider.close(resolve));
		rmSync(dataDir, { recursive: true, force: true });
		rmSync(chromeDir, { recursive: true, force: true });
	}
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
