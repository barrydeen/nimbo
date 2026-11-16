import { Marked, type Tokens } from 'marked';
import { getDefaultSanitizer, isSafeUrl, type Sanitizer } from './sanitize';

/**
 * Markdown rendering for the vault preview.
 *
 * - Raw HTML is disabled by escaping the `html` token instead of emitting it.
 * - Links and images are validated against an explicit protocol allowlist.
 * - Wikilinks are rendered by a Marked inline extension so aliases are escaped
 *   and wikilink-like text inside code spans/fences is never transformed.
 * - The final HTML is always sanitized with the shared allowlist sanitizer.
 */

export function normalizeWikiTarget(raw: string): string {
	let target = raw.trim();
	// `[[Note#Heading]]` and `[[Note^block]]` resolve to the note itself.
	const fragment = target.search(/[#^]/);
	if (fragment >= 0) target = target.slice(0, fragment).trim();
	target = target.replace(/^\.?\//, '').replace(/^\/+/, '').trim();
	if (!target) return '';
	return target.endsWith('.md') ? target : `${target}.md`;
}

function escapeHtml(value: string): string {
	return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function escapeAttribute(value: string): string {
	return value
		.replace(/&/g, '&amp;')
		.replace(/"/g, '&quot;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;')
		.replace(/'/g, '&#39;');
}

type WikiToken = Tokens.Generic & {
	type: 'wikilink';
	raw: string;
	target: string;
	label: string;
};

const wikilinkExtension = {
	name: 'wikilink',
	level: 'inline' as const,
	start(src: string) {
		const index = src.indexOf('[[');
		return index < 0 ? undefined : index;
	},
	tokenizer(src: string) {
		const match = /^\[\[([^\]\n]+)\]\]/.exec(src);
		if (!match) return undefined;
		const inner = match[1];
		const bar = inner.indexOf('|');
		const target = (bar === -1 ? inner : inner.slice(0, bar)).trim();
		const label = (bar === -1 ? inner : inner.slice(bar + 1)).trim() || target;
		if (!target) return undefined;
		return {
			type: 'wikilink',
			raw: match[0],
			target,
			label
		} satisfies WikiToken;
	},
	renderer(token: WikiToken) {
		const target = normalizeWikiTarget(token.target);
		return `<a href="#" class="wikilink" data-wiki="${escapeAttribute(
			encodeURIComponent(target)
		)}">${escapeHtml(token.label)}</a>`;
	}
};

function safeLink(token: Tokens.Link, text: string): string {
	const href = (token.href ?? '').trim();
	if (!isSafeUrl(href, { allowMailto: true })) return text;
	const title = token.title ? ` title="${escapeAttribute(token.title)}"` : '';
	return `<a href="${escapeAttribute(href)}"${title} rel="noopener noreferrer">${text}</a>`;
}

function safeImage(token: Tokens.Image): string {
	const src = (token.href ?? '').trim();
	const alt = token.text ?? '';
	if (!isSafeUrl(src, { allowMailto: false })) return escapeHtml(alt);
	const title = token.title ? ` title="${escapeAttribute(token.title)}"` : '';
	return `<img src="${escapeAttribute(src)}" alt="${escapeAttribute(alt)}"${title} loading="lazy" decoding="async">`;
}

export function createMarkdownRenderer(sanitizer: Sanitizer): (markdown: string) => string {
	const marked = new Marked({ gfm: true, breaks: false, async: false });
	marked.use({ extensions: [wikilinkExtension] });
	marked.use({
		renderer: {
			html(token: Tokens.HTML | Tokens.Tag) {
				return escapeHtml(token.text);
			},
			link(this: { parser: { parseInline(tokens: Tokens.Generic[]): string } }, token: Tokens.Link) {
				return safeLink(token, this.parser.parseInline(token.tokens as Tokens.Generic[]));
			},
			image(token: Tokens.Image) {
				return safeImage(token);
			}
		}
	});

	return (markdown: string) => {
		const html = marked.parse(markdown, { async: false }) as string;
		return sanitizer.sanitizeHtml(html);
	};
}

let defaultRenderer: ((markdown: string) => string) | null = null;

export function renderMarkdown(markdown: string): string {
	if (!defaultRenderer) {
		defaultRenderer = createMarkdownRenderer(getDefaultSanitizer());
	}
	return defaultRenderer(markdown);
}
