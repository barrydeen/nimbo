export type User = { id: string; username: string; role: string };
export type Bootstrap = {
	setup_complete: boolean;
	instance_name: string;
	user: User | null;
};
export type DocMeta = { path: string; title: string; updated_at: number };
export type DocContent = { path: string; content: string; hash: string; title: string };
export type Backlinks = {
	backlinks: { path: string; title: string }[];
	links: { target: string; path: string | null; exists: boolean }[];
};
export type SearchResult = {
	path: string;
	title: string;
	snippet: string;
	score: number;
};
export type LlmSettings = {
	configured: boolean;
	base_url: string;
	model: string;
	temperature: number;
	api_key_set: boolean;
};

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
	const headers: Record<string, string> = {
		'content-type': 'application/json',
		...(init.headers as Record<string, string> | undefined)
	};
	const res = await fetch(path, { credentials: 'include', ...init, headers });
	const text = await res.text();
	let data: unknown = null;
	if (text) {
		try {
			data = JSON.parse(text);
		} catch {
			data = null;
		}
	}
	if (!res.ok) {
		const message =
			(data && typeof data === 'object' && 'error' in data
				? String((data as { error: unknown }).error)
				: null) ?? `${res.status} ${res.statusText}`;
		throw new Error(message);
	}
	return data as T;
}

export const api = {
	bootstrap: () => request<Bootstrap>('/api/bootstrap'),
	setup: (body: { instance_name: string; username: string; password: string }) =>
		request<{ instance_name: string; user: User }>('/api/setup', {
			method: 'POST',
			body: JSON.stringify(body)
		}),
	login: (body: { username: string; password: string }) =>
		request<{ user: User }>('/api/auth/login', {
			method: 'POST',
			body: JSON.stringify(body)
		}),
	logout: () => request<{ ok: boolean }>('/api/auth/logout', { method: 'POST' }),
	tree: () => request<{ documents: DocMeta[] }>('/api/docs/tree'),
	getDoc: (path: string) =>
		request<DocContent>(`/api/docs/content?path=${encodeURIComponent(path)}`),
	saveDoc: (body: { path: string; content: string; base_hash: string | null }) =>
		request<{ path: string; hash: string }>('/api/docs/content', {
			method: 'PUT',
			body: JSON.stringify(body)
		}),
	deleteDoc: (path: string) =>
		request<{ deleted: string }>(`/api/docs/content?path=${encodeURIComponent(path)}`, {
			method: 'DELETE'
		}),
	renameDoc: (from: string, to: string) =>
		request<{ path: string }>('/api/docs/rename', {
			method: 'POST',
			body: JSON.stringify({ from, to })
		}),
	backlinks: (path: string) =>
		request<Backlinks>(`/api/docs/backlinks?path=${encodeURIComponent(path)}`),
	search: (q: string) =>
		request<{ results: SearchResult[] }>(`/api/search?q=${encodeURIComponent(q)}`),
	getLlm: () => request<LlmSettings>('/api/settings/llm'),
	saveLlm: (body: {
		base_url: string;
		model: string;
		api_key?: string;
		temperature?: number;
	}) =>
		request<LlmSettings>('/api/settings/llm', {
			method: 'PUT',
			body: JSON.stringify(body)
		}),
	testLlm: () =>
		request<{ ok: boolean; reply: string }>('/api/settings/llm/test', { method: 'POST' }),
	agentAction: (body: {
		action: string;
		path?: string;
		text?: string;
		instruction?: string;
	}) =>
		request<{ action: string; text: string }>('/api/agent/action', {
			method: 'POST',
			body: JSON.stringify(body)
		})
};
