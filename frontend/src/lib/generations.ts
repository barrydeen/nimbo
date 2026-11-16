/**
 * Monotonic, target-scoped operation generations.
 *
 * Every asynchronous operation that writes shared view state takes a token
 * before awaiting and checks `isCurrent(token)` after every await, including
 * catch/finally paths. `begin` also aborts an older operation in the same
 * scope, while `invalidate` discards results without starting a replacement.
 */
export class RequestScope {
	private generation = 0;
	private controller: AbortController | null = null;

	begin(): { id: number; signal: AbortSignal } {
		this.controller?.abort();
		const controller = new AbortController();
		this.controller = controller;
		this.generation += 1;
		return { id: this.generation, signal: controller.signal };
	}

	isCurrent(id: number): boolean {
		return id === this.generation;
	}

	/** Discard in-flight results and abort the request, without a new token. */
	invalidate(): void {
		this.generation += 1;
		this.controller?.abort();
		this.controller = null;
	}

	/** Abort without changing the generation (e.g. component teardown). */
	abort(): void {
		this.controller?.abort();
		this.controller = null;
	}

	get current(): number {
		return this.generation;
	}
}

export function isAbortError(error: unknown): boolean {
	return (
		(typeof DOMException !== 'undefined' &&
			error instanceof DOMException &&
			error.name === 'AbortError') ||
		(error instanceof Error && error.name === 'AbortError')
	);
}
