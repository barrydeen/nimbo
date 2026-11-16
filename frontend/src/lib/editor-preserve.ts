/**
 * Helpers for applying provider results without deleting unprocessed text.
 *
 * The backend reports `processed_chars` as a count of Unicode scalar values
 * (code points). CodeMirror positions and JavaScript strings use UTF-16 code
 * units, so the two must be mapped explicitly before replacing a range.
 */

/** UTF-16 offset of the prefix containing `processedChars` code points. */
export function processedPrefixLength(
	text: string,
	processedChars: number | null | undefined
): number {
	if (processedChars === null || processedChars === undefined) return text.length;
	if (!Number.isFinite(processedChars) || processedChars <= 0) return 0;
	let seen = 0;
	let offset = 0;
	for (const character of text) {
		if (seen >= processedChars) break;
		offset += character.length;
		seen += 1;
	}
	return offset;
}

export type ProcessedSpan = {
	/** CodeMirror start of the text that was actually sent to the provider. */
	from: number;
	/** CodeMirror end of the processed prefix (never the unprocessed suffix). */
	to: number;
	/** Total length of the original selection/document. */
	originalLength: number;
	truncated: boolean;
};

export function processedSpan(
	text: string,
	from: number,
	processedChars: number | null | undefined,
	truncated: boolean | null | undefined
): ProcessedSpan {
	const prefixLength = processedPrefixLength(text, processedChars);
	return {
		from,
		to: from + prefixLength,
		originalLength: text.length,
		truncated: Boolean(truncated) || prefixLength < text.length
	};
}

/** Only grammar/rewrite replace a range; links/tags append at the end. */
export function isRangeAction(action: string): boolean {
	return action === 'grammar' || action === 'rewrite';
}

/**
 * A range result is stale when the document changed after the request started.
 * Applying a full-document rewrite into an unrelated cursor is never safe.
 */
export function isRangeResultStale(
	action: string,
	requestVersion: number,
	currentVersion: number
): boolean {
	return isRangeAction(action) && requestVersion !== currentVersion;
}
