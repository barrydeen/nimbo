#!/usr/bin/env node
/**
 * Sanitizer and editor-preservation controls.
 *
 * Bundles the real TypeScript modules with esbuild, evaluates them against a
 * jsdom DOM, and asserts both security controls and data-preservation
 * behavior. Run with: node scripts/check-sanitizer.mjs
 */
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { build } from 'esbuild';
import { JSDOM } from 'jsdom';

const root = path.resolve(import.meta.dirname, '..');
const modules = {
	sanitize: path.join(root, 'src/lib/sanitize.ts'),
	markdown: path.join(root, 'src/lib/markdown.ts'),
	editor: path.join(root, 'src/lib/editor-preserve.ts'),
	preview: path.join(root, 'src/lib/preview.ts')
};

const entry = Object.entries(modules)
	.map(([name, file]) => `export * as ${name} from ${JSON.stringify(file)};`)
	.join('\n');

const output = await build({
	stdin: { contents: entry, resolveDir: root, loader: 'ts' },
	bundle: true,
	format: 'esm',
	platform: 'node',
	target: 'node22',
	write: false,
	logLevel: 'silent'
});

const dir = mkdtempSync(path.join(tmpdir(), 'nimbo-sanitizer-'));
const bundled = path.join(dir, 'modules.mjs');
writeFileSync(bundled, output.outputFiles[0].text, 'utf8');

let mod;
try {
	mod = await import(pathToFileURL(bundled).href);
} finally {
	// The import is complete; the temp file can be removed before assertions.
	rmSync(dir, { recursive: true, force: true });
}

const { sanitize, markdown, editor, preview } = mod;
const dom = new JSDOM('<!doctype html><html><body></body></html>');
const sanitizer = sanitize.createSanitizer(dom.window);
const render = markdown.createMarkdownRenderer(sanitizer);

function includes(haystack, needle, label) {
	assert.ok(haystack.includes(needle), `${label}: expected to include ${JSON.stringify(needle)}`);
}
function excludes(haystack, needle, label) {
	assert.ok(!haystack.includes(needle), `${label}: must not include ${JSON.stringify(needle)}`);
}
function matches(haystack, regex, label) {
	assert.ok(regex.test(haystack), `${label}: expected to match ${regex}`);
}
function assertNoEventAttributes(html, label) {
	const container = dom.window.document.createElement('div');
	container.innerHTML = html;
	for (const node of container.querySelectorAll('*')) {
		for (const attribute of node.attributes) {
			assert.ok(
				!attribute.name.toLowerCase().startsWith('on'),
				`${label}: unexpected event attribute ${attribute.name} on <${node.nodeName.toLowerCase()}>`
			);
		}
		if (node.nodeName.toLowerCase() === 'script') {
			assert.fail(`${label}: script element survived`);
		}
	}
}

function assertNoDangerousUrls(html, label) {
	const container = dom.window.document.createElement('div');
	container.innerHTML = html;
	for (const node of container.querySelectorAll('a[href], img[src]')) {
		const value = node.getAttribute('href') ?? node.getAttribute('src') ?? '';
		assert.ok(
			sanitize.isSafeUrl(value, { allowMailto: node.nodeName.toLowerCase() === 'a' }),
			`${label}: unsafe URL survived on <${node.nodeName.toLowerCase()}>: ${value}`
		);
	}
}

// --- Markdown: raw HTML is escaped, never emitted as DOM -------------------
let out = render('<img src=x onerror="alert(1)">');
includes(out, '&lt;img', 'raw img is escaped text');
excludes(out, '<img', 'raw img element is not created');
assertNoEventAttributes(out, 'raw img');

out = render('<script>alert(1)</script>');
excludes(out.toLowerCase(), '<script', 'raw script tag is not emitted');
assertNoEventAttributes(out, 'raw script');

out = render('<svg onload="alert(1)"></svg>');
excludes(out.toLowerCase(), '<svg', 'raw svg is not emitted');
assertNoEventAttributes(out, 'raw svg');

// --- Link and media protocol allowlist ------------------------------------
out = render('[click](javascript:alert(1))');
excludes(out.toLowerCase(), 'javascript:', 'javascript link is removed');
excludes(out, 'href=', 'javascript link loses its href');

out = render('[click](data:text/html;base64,PHNjcmlwdD4=)');
excludes(out.toLowerCase(), 'data:', 'data link is removed');
excludes(out, 'href=', 'data link loses its href');

out = render('[click](ftp://example.com/file)');
excludes(out, 'href="ftp:', 'unsupported scheme is removed');

out = render('[ok](https://example.com/path?q=1)\n\n[rel](./notes/a.md)\n\n[mail](mailto:a@example.com)');
includes(out, 'href="https://example.com/path?q=1"', 'https link is kept');
includes(out, 'href="./notes/a.md"', 'relative link is kept');
includes(out, 'href="mailto:a@example.com"', 'mailto link is kept');

out = render('![x](data:image/svg+xml,<svg onload=alert(1)>)');
excludes(out.toLowerCase(), '<img', 'data image is not created');
assertNoDangerousUrls(out, 'data image');
assertNoEventAttributes(out, 'data image');

out = render('[x](https://example.com "a\\" onmouseover=alert(1)")');
assertNoEventAttributes(out, 'title injection');

// --- Wikilinks: escaped aliases, normalized targets, not inside code ------
out = render('[[Target|<img src=x onerror=alert(1)>]]');
includes(out, 'class="wikilink"', 'wikilink anchor is rendered');
includes(out, '&lt;img', 'alias HTML is escaped');
excludes(out, '<img', 'alias cannot create an element');
assertNoEventAttributes(out, 'wikilink alias');

out = render('[[Note|Alias]]');
includes(out, 'data-wiki="Note.md"', 'wikilink target is normalized');
includes(out, '>Alias</a>', 'wikilink alias is escaped text');

out = render('[[Note#Heading]]');
includes(out, 'data-wiki="Note.md"', 'heading fragment is stripped consistently');

out = render('`[[Inline code]]`');
excludes(out, 'class="wikilink"', 'wikilink inside a code span is not transformed');
includes(out, '[[Inline code]]', 'code span keeps the literal wikilink');

out = render('```\n[[Fenced code]]\n```');
excludes(out, 'class="wikilink"', 'wikilink inside a fence is not transformed');
includes(out, '[[Fenced code]]', 'fenced code keeps the literal wikilink');

// --- Search snippets: only <mark> survives --------------------------------
out = sanitizer.sanitizeSnippet('before <mark>hit</mark> after');
includes(out, '<mark>hit</mark>', 'safe mark highlight is preserved');
includes(out, 'before ', 'snippet text is preserved');

out = sanitizer.sanitizeSnippet('<img src=x onerror=alert(1)><mark>hit</mark>');
excludes(out.toLowerCase(), '<img', 'snippet image is removed');
excludes(out.toLowerCase(), 'onerror', 'snippet event handler is removed');
includes(out, '<mark>hit</mark>', 'snippet highlight still works after an attack');

out = sanitizer.sanitizeSnippet('<mark onclick="alert(1)">hit</mark>');
includes(out, '<mark>hit</mark>', 'mark attributes are stripped');
excludes(out.toLowerCase(), 'onclick', 'mark cannot carry an event handler');

out = sanitizer.sanitizeSnippet('a <script>alert(1)</script> b');
excludes(out.toLowerCase(), '<script', 'snippet script tag is removed');

out = sanitizer.sanitizeHtml('<strong>ok</strong><img src="https://example.com/a.png" alt="a">');
includes(out, '<strong>ok</strong>', 'safe formatting survives the sanitizer');
includes(out, '<img src="https://example.com/a.png"', 'safe image survives');

// --- URL policy unit controls --------------------------------------------
assert.equal(sanitize.isSafeUrl('https://example.com'), true);
assert.equal(sanitize.isSafeUrl('./note.md'), true);
assert.equal(sanitize.isSafeUrl('#heading'), true);
assert.equal(sanitize.isSafeUrl('javascript:alert(1)'), false);
assert.equal(sanitize.isSafeUrl('data:text/html,x'), false);
assert.equal(sanitize.isSafeUrl('java\nscript:alert(1)'), false);

// --- Editor preservation: code points -> UTF-16 --------------------------
const emoji = '😀';
const astral = emoji.repeat(8005) + 'SUFFIX';
const mapped = editor.processedSpan(astral, 0, 8000, true);
assert.equal(mapped.to, 16000, '8000 code points before astral characters map to 16000 UTF-16 units');
assert.equal(astral.slice(mapped.to), emoji.repeat(5) + 'SUFFIX', 'unprocessed suffix is untouched');
assert.equal(editor.processedPrefixLength('abcdef', 4), 4);
assert.equal(editor.processedPrefixLength('abcdef', null), 6);
assert.equal(editor.processedPrefixLength('abcdef', 99), 6);
assert.equal(editor.isRangeResultStale('grammar', 1, 2), true);
assert.equal(editor.isRangeResultStale('grammar', 2, 2), false);
assert.equal(editor.isRangeResultStale('links', 1, 2), false);

const ascii = 'a'.repeat(10_000);
const asciiSpan = editor.processedSpan(ascii, 100, 8000, true);
assert.equal(asciiSpan.to, 8100, 'ASCII range is offset from the selection start');
const replaced = ascii.slice(0, asciiSpan.to) + 'FIXED' + ascii.slice(asciiSpan.to);
assert.ok(replaced.endsWith('a'.repeat(1900)), 'text after the processed prefix is preserved');

// --- Preview prefix + code point truncation ------------------------------
const truncated = preview.truncateCodePoints(emoji.repeat(3), 2);
assert.equal(truncated.text, emoji.repeat(2));
assert.equal(truncated.truncated, true);

const response = new Response('abcdefghij', { status: 200 });
const prefix = await preview.readTextPrefix(response, 4);
assert.equal(prefix.text, 'abcd');
assert.equal(prefix.truncated, true);

const small = await preview.readTextPrefix(new Response('abc'), 8);
assert.equal(small.text, 'abc');
assert.equal(small.truncated, false);

console.log('sanitizer/editor controls passed');
