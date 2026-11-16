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
	vision_model: string | null;
	describe_images: boolean;
};

export type FileNode = {
	id: string;
	parent_id: string;
	kind: 'file' | 'folder';
	name: string;
	mime: string | null;
	size: number;
	created_at?: number;
	updated_at?: number;
	has_thumb?: boolean;
	index_status?: string;
	vision_status?: string;
	path?: string;
	snippet?: string;
};

export type Breadcrumb = { id: string; name: string };

export type FileListing = {
	folder: { id: string; name: string; parent_id: string };
	breadcrumbs: Breadcrumb[];
	items: FileNode[];
};

export type FileVision = {
	status: string;
	description: string | null;
	text: string | null;
	tags: string[];
	model: string | null;
	at: number | null;
	error: string | null;
};

export type FileDetails = {
	item: FileNode & {
		sha256: string | null;
		index_error: string | null;
		metadata: Record<string, unknown>;
		text_preview: string | null;
		vision: FileVision;
	};
	path: string;
	breadcrumbs: Breadcrumb[];
	shares: ShareLink[];
};

export type ShareLink = {
	id: string;
	token: string;
	url: string;
	target_type: 'file' | 'folder';
	target_id: string;
	target_name: string | null;
	target_path: string | null;
	created_by: string | null;
	created_at: number;
	expires_at: number | null;
	revoked_at: number | null;
	last_accessed_at: number | null;
	access_count: number;
	status: 'active' | 'expired' | 'revoked';
};

export type FilesStats = {
	files: number;
	folders: number;
	images: number;
	bytes: number;
	active_shares: number;
};

export type DeviceToken = {
	id: string;
	name: string;
	created_at: number;
	last_used_at: number | null;
	expires_at: number | null;
	revoked_at: number | null;
	status: 'active' | 'expired';
};

export type UploadResult = {
	uploaded: FileNode[];
	failed: { name: string; error: string }[];
};

export type PublicShare = {
	token: string;
	instance_name: string;
	kind: 'file' | 'folder';
	name: string;
	expires_at: number | null;
	file?: FileNode;
	folder?: { id: string; name: string; parent_id: string };
	breadcrumbs?: Breadcrumb[];
	items?: FileNode[];
};

export type PublicListing = {
	folder: { id: string; name: string; parent_id: string };
	breadcrumbs: Breadcrumb[];
	items: FileNode[];
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
		vision_model?: string;
		describe_images?: boolean;
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
		}),

	listFiles: (folderId?: string | null) =>
		request<FileListing>(
			`/api/files${folderId ? `?folder_id=${encodeURIComponent(folderId)}` : ''}`
		),
	recentFiles: () => request<{ items: FileNode[] }>('/api/files/recent'),
	createFolder: (body: { parent_id?: string | null; name: string }) =>
		request<{ item: FileNode }>('/api/files/folders', {
			method: 'POST',
			body: JSON.stringify(body)
		}),
	updateNode: (id: string, body: { name?: string; parent_id?: string }) =>
		request<{ item: FileNode }>(`/api/files/${id}`, {
			method: 'PATCH',
			body: JSON.stringify(body)
		}),
	deleteNode: (id: string) =>
		request<{ deleted: string; count: number }>(`/api/files/${id}`, { method: 'DELETE' }),
	fileDetails: (id: string) => request<FileDetails>(`/api/files/${id}`),
	searchFiles: (q: string) =>
		request<{ results: FileNode[] }>(`/api/files/search?q=${encodeURIComponent(q)}`),
	filesStats: () => request<FilesStats>('/api/files/stats'),
	retryVision: (id: string) =>
		request<{ ok: boolean }>(`/api/files/${id}/vision`, { method: 'POST' }),
	reindexFile: (id: string) =>
		request<{ ok: boolean }>(`/api/files/${id}/reindex`, { method: 'POST' }),

	listShares: () => request<{ shares: ShareLink[] }>('/api/shares'),
	createShare: (body: {
		target_type: 'file' | 'folder';
		target_id: string;
		expires_at?: number | null;
	}) =>
		request<{ share: ShareLink }>('/api/shares', {
			method: 'POST',
			body: JSON.stringify(body)
		}),
	updateShare: (id: string, body: { expires_at?: number | null; clear_expiry?: boolean }) =>
		request<{ share: ShareLink }>(`/api/shares/${id}`, {
			method: 'PATCH',
			body: JSON.stringify(body)
		}),
	revokeShare: (id: string) =>
		request<{ share: ShareLink }>(`/api/shares/${id}`, { method: 'DELETE' }),

	listTokens: () => request<{ items: DeviceToken[] }>('/api/tokens'),
	createToken: (name: string) =>
		request<{ token: string; item: DeviceToken }>('/api/tokens', {
			method: 'POST',
			body: JSON.stringify({ name })
		}),
	revokeToken: (id: string) =>
		request<{ ok: boolean }>(`/api/tokens/${id}`, { method: 'DELETE' }),

	publicShare: (token: string) =>
		request<PublicShare>(`/api/public/shares/${encodeURIComponent(token)}`),
	publicNodes: (token: string, folderId?: string | null) =>
		request<PublicListing>(
			`/api/public/shares/${encodeURIComponent(token)}/nodes${
				folderId ? `?folder_id=${encodeURIComponent(folderId)}` : ''
			}`
		)
};

export const fileContentUrl = (id: string) => `/api/files/${id}/content`;
export const fileDownloadUrl = (id: string) => `/api/files/${id}/download`;
export const fileThumbUrl = (id: string) => `/api/files/${id}/thumbnail`;
export const publicDownloadUrl = (token: string, id: string) =>
	`/api/public/shares/${encodeURIComponent(token)}/files/${id}/download`;
export const publicContentUrl = (token: string, id: string) =>
	`/api/public/shares/${encodeURIComponent(token)}/files/${id}/content`;
export const publicThumbUrl = (token: string, id: string) =>
	`/api/public/shares/${encodeURIComponent(token)}/files/${id}/thumbnail`;
export const publicZipUrl = (token: string, folderId?: string | null) =>
	`/api/public/shares/${encodeURIComponent(token)}/download.zip${
		folderId ? `?folder_id=${encodeURIComponent(folderId)}` : ''
	}`;

export function uploadFiles(
	folderId: string | null,
	files: File[],
	onProgress?: (percent: number, loaded: number, total: number) => void
): Promise<UploadResult> {
	return new Promise((resolve, reject) => {
		const form = new FormData();
		for (const file of files) form.append('file', file, file.name);
		const xhr = new XMLHttpRequest();
		xhr.open(
			'POST',
			`/api/files/upload${folderId ? `?folder_id=${encodeURIComponent(folderId)}` : ''}`
		);
		xhr.withCredentials = true;
		xhr.upload.onprogress = (event) => {
			if (event.lengthComputable && onProgress) {
				onProgress(Math.round((event.loaded / event.total) * 100), event.loaded, event.total);
			}
		};
		xhr.onload = () => {
			let data: unknown = null;
			try {
				data = JSON.parse(xhr.responseText);
			} catch {
				data = null;
			}
			if (xhr.status >= 200 && xhr.status < 300) {
				resolve(data as UploadResult);
			} else {
				const message =
					data && typeof data === 'object' && 'error' in data
						? String((data as { error: unknown }).error)
						: `Upload failed (${xhr.status})`;
				reject(new Error(message));
			}
		};
		xhr.onerror = () => reject(new Error('Upload failed. Check your connection.'));
		xhr.send(form);
	});
}
