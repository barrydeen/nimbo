import DOMPurify, { type Config, type WindowLike } from 'dompurify';

/**
 * Shared, allowlist-based HTML sanitizer built on DOMPurify.
 *
 * All Markdown preview and search-snippet HTML must pass through one of these
 * functions before it reaches `{@html}`. The configuration is deliberately
 * restrictive so the output is compatible with a strict Content-Security-Policy
 * (no event handlers, inline styles, object/embed/frame content or active URLs).
 */

const SAFE_SCHEME = /^([a-z][a-z0-9+.-]*):/i;

/** URL policy shared by Markdown renderers and the sanitizer hooks. */
export function isSafeUrl(value: string, options: { allowMailto?: boolean } = {}): boolean {
	const raw = value.trim();
	if (!raw) return false;
	// Reject control characters that browsers may strip before resolving a URL.
	if (/[\u0000-\u001f\u007f]/.test(raw)) return false;
	const match = SAFE_SCHEME.exec(raw);
	if (!match) return true; // relative, fragment or query-only URL
	const scheme = match[1].toLowerCase();
	if (scheme === 'http' || scheme === 'https') return true;
	return scheme === 'mailto' && options.allowMailto !== false;
}

const SAFE_URI_REGEXP = /^(?:(?:https?|mailto):|[^a-z]|[a-z+.\-]+(?:[^a-z+.\-:]|$))/i;

const MARKDOWN_TAGS = [
	'a',
	'abbr',
	'b',
	'blockquote',
	'br',
	'caption',
	'code',
	'dd',
	'del',
	'details',
	'div',
	'dl',
	'dt',
	'em',
	'h1',
	'h2',
	'h3',
	'h4',
	'h5',
	'h6',
	'hr',
	'i',
	'img',
	'li',
	'mark',
	'ol',
	'p',
	'pre',
	's',
	'span',
	'strong',
	'sub',
	'summary',
	'sup',
	'table',
	'tbody',
	'td',
	'tfoot',
	'th',
	'thead',
	'tr',
	'u',
	'ul'
];

const MARKDOWN_ATTRS = [
	'alt',
	'class',
	'colspan',
	'data-wiki',
	'decoding',
	'height',
	'href',
	'loading',
	'rel',
	'rowspan',
	'src',
	'start',
	'title',
	'width'
];

const MARKDOWN_CONFIG: Config = {
	ALLOWED_TAGS: MARKDOWN_TAGS,
	ALLOWED_ATTR: MARKDOWN_ATTRS,
	ALLOWED_URI_REGEXP: SAFE_URI_REGEXP,
	ALLOW_DATA_ATTR: false,
	ADD_ATTR: ['data-wiki'],
	ALLOW_ARIA_ATTR: false,
	FORBID_TAGS: [
		'audio',
		'base',
		'button',
		'canvas',
		'embed',
		'form',
		'frame',
		'frameset',
		'iframe',
		'input',
		'link',
		'meta',
		'object',
		'option',
		'param',
		'picture',
		'select',
		'source',
		'style',
		'svg',
		'textarea',
		'track',
		'video'
	],
	FORBID_ATTR: ['action', 'formaction', 'poster', 'srcset', 'style'],
	KEEP_CONTENT: true,
};

const SNIPPET_CONFIG: Config = {
	ALLOWED_TAGS: ['mark'],
	ALLOWED_ATTR: [],
	ALLOW_DATA_ATTR: false,
	ALLOW_ARIA_ATTR: false,
	KEEP_CONTENT: true,
};

export type Sanitizer = {
	sanitizeHtml(html: string): string;
	sanitizeSnippet(html: string): string;
};

/** Build a sanitizer for a specific DOM window (browser, jsdom, ...). */
export function createSanitizer(windowLike: Window & typeof globalThis): Sanitizer {
	const purifier = DOMPurify(windowLike as unknown as WindowLike);

	// Defence in depth: re-check URLs after DOMPurify's own URI filtering and
	// harden links against reverse-tabnabbing. No attribute may reintroduce an
	// executable scheme.
	purifier.addHook('afterSanitizeAttributes', (node) => {
		const tag = node.nodeName.toLowerCase();
		if (tag === 'a') {
			const href = node.getAttribute('href');
			if (href !== null && !isSafeUrl(href, { allowMailto: true })) {
				node.removeAttribute('href');
			}
			node.setAttribute('rel', 'noopener noreferrer');
		} else if (tag === 'img') {
			const src = node.getAttribute('src');
			if (src !== null && !isSafeUrl(src, { allowMailto: false })) {
				node.removeAttribute('src');
			}
		}
	});

	return {
		sanitizeHtml(html: string): string {
			return purifier.sanitize(html, MARKDOWN_CONFIG) as string;
		},
		sanitizeSnippet(html: string): string {
			return purifier.sanitize(html, SNIPPET_CONFIG) as string;
		}
	};
}

let defaultSanitizer: Sanitizer | null = null;

export function getDefaultSanitizer(): Sanitizer {
	if (!defaultSanitizer) {
		if (typeof window === 'undefined') {
			throw new Error('HTML sanitization requires a browser DOM');
		}
		defaultSanitizer = createSanitizer(window);
	}
	return defaultSanitizer;
}

/** Sanitize rendered Markdown before inserting it with `{@html}`. */
export function sanitizeHtml(html: string): string {
	return getDefaultSanitizer().sanitizeHtml(html);
}

/**
 * Sanitize a search snippet. The server is allowed to emit only `<mark>`
 * delimiters around escaped text; every other tag and all attributes are
 * removed. Text content is preserved so an attack payload cannot erase the
 * surrounding excerpt.
 */
export function sanitizeSnippet(html: string): string {
	return getDefaultSanitizer().sanitizeSnippet(html);
}
