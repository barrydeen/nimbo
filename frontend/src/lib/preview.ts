/**
 * Bounded text-preview helpers.
 *
 * Previews never trust `Content-Length` alone: they either use a byte range or
 * stop and cancel the response stream after a fixed prefix. This keeps a very
 * large upload from being downloaded and decoded in full just to display a
 * short preview.
 */

export const PREVIEW_MAX_BYTES = 2 * 1024 * 1024;
export const PREVIEW_PREFIX_BYTES = 256 * 1024;
export const PREVIEW_MAX_CHARS = 120_000;

export type PreviewState = 'idle' | 'loading' | 'loaded' | 'too-large' | 'error' | 'empty';

export function truncateCodePoints(text: string, maxChars: number): {
	text: string;
	truncated: boolean;
} {
	if (maxChars <= 0) return { text: '', truncated: text.length > 0 };
	let out = '';
	let count = 0;
	for (const character of text) {
		if (count >= maxChars) return { text: out, truncated: true };
		out += character;
		count += 1;
	}
	return { text: out, truncated: false };
}

export async function readTextPrefix(
	response: Response,
	maxBytes = PREVIEW_PREFIX_BYTES
): Promise<{ text: string; truncated: boolean }> {
	if (!response.body) {
		const full = await response.text();
		const limited = truncateCodePoints(full, PREVIEW_MAX_CHARS);
		return { text: limited.text, truncated: limited.truncated };
	}

	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	let truncated = false;
	try {
		while (total < maxBytes) {
			const { done, value } = await reader.read();
			if (done) break;
			if (!value || value.byteLength === 0) continue;
			const remaining = maxBytes - total;
			if (value.byteLength > remaining) {
				chunks.push(value.subarray(0, remaining));
				total += remaining;
				truncated = true;
				break;
			}
			chunks.push(value);
			total += value.byteLength;
		}
	} finally {
		if (truncated) await reader.cancel().catch(() => undefined);
		try {
			reader.releaseLock();
		} catch {
			// The stream may already be closed or errored; nothing to release.
		}
	}

	const merged = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		merged.set(chunk, offset);
		offset += chunk.byteLength;
	}
	const decoded = new TextDecoder().decode(merged);
	const limited = truncateCodePoints(decoded, PREVIEW_MAX_CHARS);
	return { text: limited.text, truncated: truncated || limited.truncated };
}
