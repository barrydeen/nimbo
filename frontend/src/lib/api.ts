export type User = { id: string; username: string; role: string };
export type Bootstrap = {
	setup_complete: boolean;
	instance_name: string;
	user: User | null;
	/** Present when the server requires NIMBO_SETUP_TOKEN during setup. */
	setup_token_required?: boolean;
};
export type InstanceInfo = {
	setup_complete?: boolean;
	instance_name?: string;
	max_upload_bytes?: number;
	backup_scope?: boolean;
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
export type ProviderSettings = {
	id: string;
	name: string;
	base_url: string;
	api_key_set: boolean;
	models: string[];
};

export type ModelRef = {
	provider_id: string;
	model: string;
};

export type LlmSettings = {
	configured: boolean;
	providers: ProviderSettings[];
	document_model: ModelRef | null;
	indexing_model: ModelRef | null;
	temperature: number;
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
	sha256?: string | null;
};

export type Breadcrumb = { id: string; name: string };

export type FileListing = {
	folder: { id: string; name: string; parent_id: string };
	breadcrumbs: Breadcrumb[];
	items: FileNode[];
	next_offset?: number | null;
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

export type TokenScope = 'full' | 'backup';

export type DeviceToken = {
	id: string;
	name: string;
	created_at: number;
	last_used_at: number | null;
	expires_at: number | null;
	revoked_at: number | null;
	status: 'active' | 'expired' | 'revoked';
	/** Older servers omit scope; they behave as full-account tokens. */
	scope?: TokenScope;
	backup_root_id?: string | null;
};

export type UploadResult = {
	uploaded: FileNode[];
	failed: { name: string; error: string }[];
};

/**
 * Minimal public-share node. The public API intentionally omits parent IDs,
 * modification timestamps and all private indexing metadata.
 */
export type PublicFileNode = {
	id: string;
	kind: 'file' | 'folder';
	name: string;
	mime: string | null;
	size: number;
	has_thumb?: boolean;
};

export type PublicShare = {
	token: string;
	instance_name: string;
	kind: 'file' | 'folder';
	name: string;
	expires_at: number | null;
	file?: PublicFileNode;
	folder?: { id: string; name: string; parent_id?: string };
	breadcrumbs?: Breadcrumb[];
	items?: PublicFileNode[];
	truncated?: boolean;
};

export type PublicListing = {
	folder: { id: string; name: string; parent_id?: string };
	breadcrumbs: Breadcrumb[];
	items: PublicFileNode[];
	truncated?: boolean;
};

/** Structured API failure, including the HTTP status needed for auth policy. */
export class ApiError extends Error {
	status: number;

	constructor(message: string, status: number) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
	}
}

export function isUnauthorized(error: unknown): boolean {
	return error instanceof ApiError && error.status === 401;
}

export function errorMessage(error: unknown, fallback = 'Something went wrong'): string {
	return error instanceof Error && error.message ? error.message : fallback;
}

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
		throw new ApiError(message, res.status);
	}
	return data as T;
}

export const api = {
	bootstrap: () => request<Bootstrap>('/api/bootstrap'),
	instance: () => request<InstanceInfo>('/api/instance'),
	setup: (body: {
		instance_name: string;
		username: string;
		password: string;
		setup_token?: string;
	}) =>
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
	getDoc: (path: string, signal?: AbortSignal) =>
		request<DocContent>(`/api/docs/content?path=${encodeURIComponent(path)}`, { signal }),
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
	backlinks: (path: string, signal?: AbortSignal) =>
		request<Backlinks>(`/api/docs/backlinks?path=${encodeURIComponent(path)}`, { signal }),
	search: (q: string, signal?: AbortSignal) =>
		request<{ results: SearchResult[] }>(`/api/search?q=${encodeURIComponent(q)}`, { signal }),
	getLlm: () => request<LlmSettings>('/api/settings/llm'),
	saveLlm: (body: {
		providers: {
			id: string;
			name?: string;
			base_url: string;
			models: string[];
			api_key?: string;
			clear_api_key?: boolean;
			confirm_reuse_api_key?: boolean;
		}[];
		document_model?: ModelRef | null;
		indexing_model?: ModelRef | null;
		temperature?: number;
		describe_images?: boolean;
	}) =>
		request<LlmSettings>('/api/settings/llm', {
			method: 'PUT',
			body: JSON.stringify(body)
		}),
	listLlmModels: (body: {
		provider_id?: string;
		base_url: string;
		api_key?: string;
	}) =>
		request<{ models: string[] }>('/api/settings/llm/models', {
			method: 'POST',
			body: JSON.stringify(body)
		}),
	testLlm: () =>
		request<{ ok: boolean; reply: string }>('/api/settings/llm/test', { method: 'POST' }),
	agentAction: (
		body: {
			action: string;
			path?: string;
			text?: string;
			instruction?: string;
		},
		signal?: AbortSignal
	) =>
		request<{ action: string; text: string; processed_chars?: number; truncated?: boolean }>(
			'/api/agent/action',
			{
				method: 'POST',
				body: JSON.stringify(body),
				signal
			}
		),

	listFiles: (
		folderId?: string | null,
		signal?: AbortSignal,
		offset = 0,
		limit = 500
	) => {
		const query = new URLSearchParams({ offset: String(offset), limit: String(limit) });
		if (folderId) query.set('folder_id', folderId);
		return request<FileListing>(`/api/files?${query}`, { signal });
	},
	recentFiles: (signal?: AbortSignal) => request<{ items: FileNode[] }>('/api/files/recent', { signal }),
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
	fileDetails: (id: string, signal?: AbortSignal) =>
		request<FileDetails>(`/api/files/${id}`, { signal }),
	searchFiles: (q: string, signal?: AbortSignal) =>
		request<{ results: FileNode[] }>(`/api/files/search?q=${encodeURIComponent(q)}`, { signal }),
	filesStats: (signal?: AbortSignal) => request<FilesStats>('/api/files/stats', { signal }),
	retryVision: (id: string) =>
		request<{ ok: boolean }>(`/api/files/${id}/vision`, { method: 'POST' }),
	reindexFile: (id: string) =>
		request<{ ok: boolean }>(`/api/files/${id}/reindex`, { method: 'POST' }),

	listShares: (signal?: AbortSignal) => request<{ shares: ShareLink[] }>('/api/shares', { signal }),
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
	createToken: (name: string, scope: TokenScope = 'full') =>
		request<{ token: string; item: DeviceToken }>('/api/tokens', {
			method: 'POST',
			body: JSON.stringify({ name, scope })
		}),
	revokeToken: (id: string) =>
		request<{ ok: boolean }>(`/api/tokens/${id}`, { method: 'DELETE' }),

	publicShare: (token: string, signal?: AbortSignal) =>
		request<PublicShare>(`/api/public/shares/${encodeURIComponent(token)}`, { signal }),
	publicNodes: (token: string, folderId?: string | null, signal?: AbortSignal) =>
		request<PublicListing>(
			`/api/public/shares/${encodeURIComponent(token)}/nodes${
				folderId ? `?folder_id=${encodeURIComponent(folderId)}` : ''
			}`,
			{ signal }
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

export class UploadError extends ApiError {
	/** True when the server may have committed the file before failing. */
	ambiguous: boolean;

	constructor(message: string, status: number, ambiguous = false) {
		super(message, status);
		this.name = 'UploadError';
		this.ambiguous = ambiguous;
	}
}

export type UploadOptions = {
	signal?: AbortSignal;
	timeoutMs?: number;
	onProgress?: (percent: number) => void;
};

/** Upload one file so count/aggregate body limits and outcomes are isolated. */
export function uploadFile(
	folderId: string | null,
	file: File,
	options: UploadOptions = {}
): Promise<UploadResult> {
	return new Promise((resolve, reject) => {
		const form = new FormData();
		form.append('file', file, file.name);
		const xhr = new XMLHttpRequest();
		const timeoutMs = options.timeoutMs ?? 120_000;

		const cleanup = () => {
			options.signal?.removeEventListener('abort', onAbort);
		};
		const fail = (error: UploadError) => {
			cleanup();
			reject(error);
		};
		const onAbort = () => {
			xhr.abort();
		};

		xhr.open(
			'POST',
			`/api/files/upload${folderId ? `?folder_id=${encodeURIComponent(folderId)}` : ''}`
		);
		xhr.withCredentials = true;
		xhr.timeout = timeoutMs;
		xhr.upload.onprogress = (event) => {
			if (event.lengthComputable && options.onProgress) {
				options.onProgress(Math.round((event.loaded / event.total) * 100));
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
				cleanup();
				resolve(data as UploadResult);
				return;
			}
			const message =
				data && typeof data === 'object' && 'error' in data
					? String((data as { error: unknown }).error)
					: `Upload failed (${xhr.status})`;
			fail(new UploadError(message, xhr.status, xhr.status >= 500));
		};
		xhr.onerror = () =>
			fail(new UploadError('Upload failed. Check your connection and the folder contents.', 0, true));
		xhr.ontimeout = () =>
			fail(
				new UploadError(
					`Upload timed out after ${Math.round(timeoutMs / 1000)}s. The file may already be stored; refresh the folder before retrying.`,
					0,
					true
				)
			);
		xhr.onabort = () => {
			cleanup();
			reject(new DOMException('Upload cancelled', 'AbortError'));
		};

		if (options.signal) {
			if (options.signal.aborted) {
				reject(new DOMException('Upload cancelled', 'AbortError'));
				return;
			}
			options.signal.addEventListener('abort', onAbort, { once: true });
		}
		xhr.send(form);
	});
}

/**
 * Compatibility helper for callers that still pass a file list. It uploads one
 * file per request instead of relying on a single aggregate multipart body.
 */
export async function uploadFiles(
	folderId: string | null,
	files: File[],
	onProgress?: (percent: number, loaded: number, total: number) => void,
	options: UploadOptions = {}
): Promise<UploadResult> {
	const uploaded: FileNode[] = [];
	const failed: { name: string; error: string }[] = [];
	for (let index = 0; index < files.length; index += 1) {
		const file = files[index];
		try {
			const result = await uploadFile(folderId, file, {
				...options,
				onProgress: (percent) => {
					const overall = Math.round(((index + percent / 100) / files.length) * 100);
					onProgress?.(overall, index, files.length);
				}
			});
			uploaded.push(...(result.uploaded ?? []));
			failed.push(...(result.failed ?? []));
		} catch (err) {
			failed.push({
				name: file.name,
				error: err instanceof Error ? err.message : 'Upload failed'
			});
		}
	}
	return { uploaded, failed };
}
