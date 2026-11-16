<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import qrcode from 'qrcode-generator';
	import {
		api,
		errorMessage,
		isUnauthorized,
		type DeviceToken,
		type LlmSettings,
		type ModelRef,
		type TokenScope
	} from '$lib/api';

	const MAX_PROVIDERS = 10;
	const MAX_SELECTED_MODELS = 200;

	type ProviderDraft = {
		id: string;
		name: string;
		baseUrl: string;
		apiKey: string;
		apiKeySet: boolean;
		clearApiKey: boolean;
		savedBaseUrl: string;
		availableModels: string[];
		selectedModels: string[];
		modelFilter: string;
		manualModel: string;
		fetching: boolean;
		fetchError: string;
		fetchStatus: string;
	};

	type ModelOption = ModelRef & { label: string };

	let activeTab: 'providers' | 'tokens' = 'providers';
	let loading = true;
	let error = '';
	let status = '';
	let saving = false;
	let testing = false;
	let configured = false;
	let providers: ProviderDraft[] = [];
	let documentModel: ModelRef | null = null;
	let indexingModel: ModelRef | null = null;
	let temperature = 0.7;
	let describeImages = true;
	let modelOptions: ModelOption[] = [];

	let tokens: DeviceToken[] = [];
	let tokenName = '';
	let tokenScope: TokenScope = 'backup';
	let newToken = '';
	let tokenError = '';
	let tokenStatus = '';
	let tokenBusy = false;

	let qrOpen = false;
	let qrStep: 'name' | 'qr' = 'name';
	let qrName = 'My phone';
	let qrBusy = false;
	let qrError = '';
	let qrPayload = '';
	let qrTokenId = '';
	let qrCanvas: HTMLCanvasElement | undefined;

	$: modelOptions = providers.flatMap((provider) =>
		provider.selectedModels.map((model) => ({
			provider_id: provider.id,
			model,
			label: `${providerLabel(provider)} — ${model}`
		}))
	);
	$: documentModelKey = documentModel ? modelRefKey(documentModel) : '';
	$: indexingModelKey = indexingModel ? modelRefKey(indexingModel) : '';
	$: documentModelValid =
		providers.length === 0
			? !documentModel
			: !!documentModel && modelOptions.some((option) => refMatches(documentModel, option));
	$: indexingModelValid =
		providers.length === 0
			? !indexingModel
			: !indexingModel || modelOptions.some((option) => refMatches(indexingModel, option));
	$: canSave = !saving && documentModelValid && indexingModelValid;

	function formatWhen(ts: number | null) {
		return ts ? new Date(ts * 1000).toLocaleString() : 'Never';
	}

	function message(err: unknown): string {
		return errorMessage(err);
	}

	function scopeLabel(token: DeviceToken): string {
		return token.scope === 'backup' ? 'Backup only · Phone Backups' : 'Full account';
	}

	function newId(): string {
		if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
			return crypto.randomUUID();
		}
		return `p-${Date.now()}-${Math.random().toString(36).slice(2)}`;
	}

	function newProviderDraft(): ProviderDraft {
		return {
			id: newId(),
			name: '',
			baseUrl: '',
			apiKey: '',
			apiKeySet: false,
			clearApiKey: false,
			savedBaseUrl: '',
			availableModels: [],
			selectedModels: [],
			modelFilter: '',
			manualModel: '',
			fetching: false,
			fetchError: '',
			fetchStatus: ''
		};
	}

	function providerLabel(provider: ProviderDraft): string {
		return provider.name.trim() || originOf(provider.baseUrl) || 'New provider';
	}

	function originOf(value: string): string {
		try {
			return new URL(value).origin;
		} catch {
			return '';
		}
	}

	function validBaseUrl(value: string): boolean {
		const trimmed = value.trim();
		return (
			(trimmed.startsWith('http://') || trimmed.startsWith('https://')) && !!originOf(trimmed)
		);
	}

	function modelRefKey(ref: ModelRef): string {
		return JSON.stringify([ref.provider_id, ref.model]);
	}

	function parseModelRefKey(value: string): ModelRef | null {
		if (!value) return null;
		try {
			const parsed = JSON.parse(value);
			if (
				Array.isArray(parsed) &&
				parsed.length === 2 &&
				typeof parsed[0] === 'string' &&
				typeof parsed[1] === 'string'
			) {
				return { provider_id: parsed[0], model: parsed[1] };
			}
		} catch {
			/* ignore malformed option values */
		}
		return null;
	}

	function refMatches(ref: ModelRef | null, option: ModelRef): boolean {
		return !!ref && ref.provider_id === option.provider_id && ref.model === option.model;
	}

	function inputValue(event: Event): string {
		return (event.currentTarget as HTMLInputElement).value;
	}

	function checkboxValue(event: Event): boolean {
		return (event.currentTarget as HTMLInputElement).checked;
	}

	function selectValue(event: Event): string {
		return (event.currentTarget as HTMLSelectElement).value;
	}

	function updateProvider(id: string, patch: Partial<ProviderDraft>) {
		providers = providers.map((provider) =>
			provider.id === id ? { ...provider, ...patch } : provider
		);
	}

	function addProvider() {
		if (providers.length >= MAX_PROVIDERS) {
			error = `At most ${MAX_PROVIDERS} providers are supported.`;
			return;
		}
		error = '';
		status = '';
		providers = [...providers, newProviderDraft()];
	}

	function removeProvider(id: string) {
		const provider = providers.find((candidate) => candidate.id === id);
		if (!provider) return;
		const inUse = documentModel?.provider_id === id || indexingModel?.provider_id === id;
		if (inUse && !confirm(`Remove ${providerLabel(provider)}? Its model roles will be cleared.`)) {
			return;
		}
		providers = providers.filter((candidate) => candidate.id !== id);
		if (documentModel?.provider_id === id) documentModel = null;
		if (indexingModel?.provider_id === id) indexingModel = null;
		error = '';
		status = '';
	}

	function filteredModels(provider: ProviderDraft): string[] {
		const filter = provider.modelFilter.trim().toLowerCase();
		if (!filter) return provider.availableModels;
		return provider.availableModels.filter((model) => model.toLowerCase().includes(filter));
	}

	function toggleModel(id: string, model: string) {
		let limitHit = false;
		providers = providers.map((provider) => {
			if (provider.id !== id) return provider;
			if (provider.selectedModels.includes(model)) {
				return {
					...provider,
					selectedModels: provider.selectedModels.filter((selected) => selected !== model)
				};
			}
			if (provider.selectedModels.length >= MAX_SELECTED_MODELS) {
				limitHit = true;
				return provider;
			}
			return { ...provider, selectedModels: [...provider.selectedModels, model] };
		});
		if (limitHit) {
			error = `A provider may have at most ${MAX_SELECTED_MODELS} selected models.`;
		}
	}

	function selectAllModels(id: string) {
		const provider = providers.find((candidate) => candidate.id === id);
		if (!provider) return;
		const visible = filteredModels(provider);
		const selected = Array.from(new Set([...provider.selectedModels, ...visible])).slice(
			0,
			MAX_SELECTED_MODELS
		);
		updateProvider(id, { selectedModels: selected });
	}

	function clearModels(id: string) {
		updateProvider(id, { selectedModels: [] });
	}

	function addManualModel(id: string) {
		const provider = providers.find((candidate) => candidate.id === id);
		if (!provider) return;
		const model = provider.manualModel.trim();
		if (!model) return;
		if (model.length > 200) {
			updateProvider(id, { fetchError: 'Model IDs must be at most 200 characters.' });
			return;
		}
		const availableModels = provider.availableModels.includes(model)
			? provider.availableModels
			: [...provider.availableModels, model].sort((a, b) => a.localeCompare(b));
		let selectedModels = provider.selectedModels;
		if (!selectedModels.includes(model)) {
			if (selectedModels.length >= MAX_SELECTED_MODELS) {
				updateProvider(id, {
					availableModels,
					manualModel: '',
					fetchError: `Model added to the list, but ${MAX_SELECTED_MODELS} models are already selected.`
				});
				return;
			}
			selectedModels = [...selectedModels, model];
		}
		updateProvider(id, {
			availableModels,
			selectedModels,
			manualModel: '',
			fetchError: '',
			fetchStatus: `Added ${model}.`
		});
	}

	async function fetchModels(id: string) {
		const provider = providers.find((candidate) => candidate.id === id);
		if (!provider) return;
		if (!validBaseUrl(provider.baseUrl)) {
			updateProvider(id, {
				fetchError: 'Enter a base URL starting with http:// or https://.'
			});
			return;
		}
		updateProvider(id, { fetching: true, fetchError: '', fetchStatus: '' });
		try {
			const result = await api.listLlmModels({
				...(provider.apiKeySet ? { provider_id: provider.id } : {}),
				base_url: provider.baseUrl.trim(),
				...(provider.apiKey.trim() && !provider.clearApiKey
					? { api_key: provider.apiKey.trim() }
					: {})
			});
			const availableModels = result.models;
			const selectedModels = provider.selectedModels.filter((model) =>
				availableModels.includes(model)
			);
			updateProvider(id, {
				availableModels,
				selectedModels,
				fetching: false,
				fetchError: '',
				fetchStatus: `Found ${availableModels.length} model${availableModels.length === 1 ? '' : 's'}.`
			});
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			updateProvider(id, { fetching: false, fetchError: message(err) });
		}
	}

	function providerNeedsKeyConfirmation(provider: ProviderDraft): boolean {
		return (
			provider.apiKeySet &&
			!provider.apiKey.trim() &&
			!provider.clearApiKey &&
			!!provider.savedBaseUrl &&
			originOf(provider.baseUrl) !== originOf(provider.savedBaseUrl)
		);
	}

	function onDocumentModelChange(event: Event) {
		documentModel = parseModelRefKey(selectValue(event));
	}

	function onIndexingModelChange(event: Event) {
		indexingModel = parseModelRefKey(selectValue(event));
	}

	function applySettings(settings: LlmSettings) {
		providers = settings.providers.map((provider) => ({
			id: provider.id,
			name: provider.name,
			baseUrl: provider.base_url,
			apiKey: '',
			apiKeySet: provider.api_key_set,
			clearApiKey: false,
			savedBaseUrl: provider.base_url,
			availableModels: [...provider.models],
			selectedModels: [...provider.models],
			modelFilter: '',
			manualModel: '',
			fetching: false,
			fetchError: '',
			fetchStatus: ''
		}));
		documentModel = settings.document_model;
		indexingModel = settings.indexing_model;
		temperature = settings.temperature;
		describeImages = settings.describe_images;
		configured = settings.configured;
	}

	async function save() {
		error = '';
		status = '';

		for (const provider of providers) {
			if (!validBaseUrl(provider.baseUrl)) {
				error = `Enter a base URL starting with http:// or https:// for ${providerLabel(provider)}.`;
				return;
			}
		}
		if (providers.length > 0 && !documentModel) {
			error = 'Choose a document agent model.';
			return;
		}
		if (!documentModelValid) {
			error = 'The document agent model must be one of the selected models.';
			return;
		}
		if (!indexingModelValid) {
			error = 'The file indexing model must be one of the selected models.';
			return;
		}

		const needsConfirm = providers.filter(providerNeedsKeyConfirmation);
		if (needsConfirm.length > 0) {
			const labels = needsConfirm.map(providerLabel).join(', ');
			const confirmed = confirm(
				`The provider origin changed for ${labels}. Reuse the stored API key(s) with the new origin(s)? Cancel to keep the stored key(s) unused and enter a new key.`
			);
			if (!confirmed) {
				error =
					'Not saved: confirm key reuse or enter a new API key. The stored key was not sent to the new provider.';
				return;
			}
		}

		saving = true;
		try {
			const saved = await api.saveLlm({
				providers: providers.map((provider) => ({
					id: provider.id,
					name: provider.name.trim(),
					base_url: provider.baseUrl.trim(),
					models: provider.selectedModels,
					...(provider.apiKey.trim() && !provider.clearApiKey
						? { api_key: provider.apiKey.trim() }
						: {}),
					...(provider.clearApiKey ? { clear_api_key: true } : {}),
					...(needsConfirm.some((confirmed) => confirmed.id === provider.id)
						? { confirm_reuse_api_key: true }
						: {})
				})),
				document_model: documentModel,
				indexing_model: indexingModel,
				temperature,
				describe_images: describeImages
			});
			applySettings(saved);
			if (!saved.configured) {
				status = 'Saved. No document agent model is configured.';
			} else {
				const documentLabel = saved.document_model?.model ?? 'the selected model';
				status = saved.indexing_model
					? `Saved. Document agent uses ${documentLabel}; file indexing uses ${saved.indexing_model.model}.`
					: `Saved. Document agent uses ${documentLabel}; file indexing uses the document agent model.`;
			}
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			error = message(err);
		} finally {
			saving = false;
		}
	}

	async function test() {
		error = '';
		status = '';
		testing = true;
		try {
			const result = await api.testLlm();
			status = `Document agent responded: ${result.reply.trim()}`;
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			error = message(err);
		} finally {
			testing = false;
		}
	}

	async function loadTokens() {
		const result = await api.listTokens();
		tokens = result.items;
	}

	async function createToken(event: SubmitEvent) {
		event.preventDefault();
		tokenError = '';
		tokenStatus = '';
		newToken = '';
		tokenBusy = true;
		try {
			const result = await api.createToken(tokenName.trim(), tokenScope);
			newToken = result.token;
			tokenName = '';
			await loadTokens();
			tokenStatus = 'Token created. Copy it now — it is shown only once.';
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			tokenError = message(err);
		} finally {
			tokenBusy = false;
		}
	}

	async function revokeToken(token: DeviceToken) {
		if (!confirm(`Revoke the device token "${token.name}"? The client will stop working.`)) {
			return;
		}
		tokenError = '';
		tokenStatus = '';
		tokenBusy = true;
		try {
			await api.revokeToken(token.id);
			await loadTokens();
			tokenStatus = `Revoked "${token.name}".`;
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			tokenError = message(err);
		} finally {
			tokenBusy = false;
		}
	}

	async function copyToken() {
		try {
			await navigator.clipboard.writeText(newToken);
			tokenStatus = 'Copied to clipboard.';
		} catch {
			tokenStatus = 'Select the token above and copy it manually.';
		}
	}

	/** The phone must reach the server at the same address this browser uses. */
	function connectPayload(token: string) {
		return `nimbo://connect?v=1&url=${encodeURIComponent(window.location.origin)}&token=${encodeURIComponent(token)}`;
	}

	function openQrModal() {
		qrOpen = true;
		qrStep = 'name';
		qrError = '';
		qrName = 'My phone';
	}

	function closeQrModal() {
		qrOpen = false;
		qrStep = 'name';
		qrPayload = '';
		qrTokenId = '';
	}

	function drawQr() {
		if (!qrCanvas || !qrPayload) return;
		const code = qrcode(0, 'M');
		code.addData(qrPayload);
		code.make();
		const count = code.getModuleCount();
		const scale = 6;
		const quiet = 4;
		const size = (count + quiet * 2) * scale;
		qrCanvas.width = size;
		qrCanvas.height = size;
		const ctx = qrCanvas.getContext('2d');
		if (!ctx) return;
		ctx.fillStyle = '#ffffff';
		ctx.fillRect(0, 0, size, size);
		ctx.fillStyle = '#10141c';
		for (let row = 0; row < count; row++) {
			for (let col = 0; col < count; col++) {
				if (code.isDark(row, col)) {
					ctx.fillRect((col + quiet) * scale, (row + quiet) * scale, scale, scale);
				}
			}
		}
	}

	$: if (qrOpen && qrStep === 'qr' && qrCanvas && qrPayload) drawQr();

	async function generateQr() {
		qrError = '';
		qrBusy = true;
		try {
			const result = await api.createToken(qrName.trim(), 'backup');
			qrPayload = connectPayload(result.token);
			qrTokenId = result.item.id;
			qrStep = 'qr';
			await loadTokens();
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			qrError = message(err);
		} finally {
			qrBusy = false;
		}
	}

	async function revokeQrToken() {
		qrBusy = true;
		try {
			await api.revokeToken(qrTokenId);
			await loadTokens();
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			// best effort — the modal closes either way
		} finally {
			qrBusy = false;
			closeQrModal();
		}
	}

	onMount(async () => {
		loading = true;
		try {
			const boot = await api.bootstrap();
			if (!boot.setup_complete) {
				await goto('/setup');
				return;
			}
			if (!boot.user) {
				await goto('/login');
				return;
			}
			if (boot.user.role !== 'admin') {
				await goto('/vault');
				return;
			}
			const settings = await api.getLlm();
			applySettings(settings);
			await loadTokens();
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			error = message(err);
		} finally {
			loading = false;
		}
	});
</script>

<div class="settings-shell">
	<div class="settings-card">
		<div class="settings-head">
			<div>
				<div class="brand">
					<span class="mark">N</span>
					<span class="name">Nimbo settings</span>
				</div>
				<h1>Settings</h1>
				<p class="sub">Configure AI providers, model roles and device access.</p>
			</div>
			<button class="btn ghost" type="button" on:click={() => goto('/vault')}>
				Back to vault
			</button>
		</div>

		{#if error}
			<div class="banner">{error}</div>
		{/if}
		{#if status}
			<div class="banner ok">{status}</div>
		{/if}

		<div class="settings-tabs" role="tablist" aria-label="Settings sections">
			<button
				class="settings-tab"
				class:active={activeTab === 'providers'}
				type="button"
				role="tab"
				id="tab-providers"
				aria-controls="panel-providers"
				aria-selected={activeTab === 'providers'}
				on:click={() => (activeTab = 'providers')}
			>
				Providers &amp; models
			</button>
			<button
				class="settings-tab"
				class:active={activeTab === 'tokens'}
				type="button"
				role="tab"
				id="tab-tokens"
				aria-controls="panel-tokens"
				aria-selected={activeTab === 'tokens'}
				on:click={() => (activeTab = 'tokens')}
			>
				Device tokens
			</button>
		</div>

		{#if loading}
			<div class="settings-panel">
				<p class="field-hint">Loading settings…</p>
			</div>
		{:else if activeTab === 'providers'}
			<div
				class="settings-panel"
				role="tabpanel"
				id="panel-providers"
				aria-labelledby="tab-providers"
			>
				<div class="section-head">
					<h2>Providers</h2>
					<button
						class="btn"
						type="button"
						on:click={addProvider}
						disabled={providers.length >= MAX_PROVIDERS}
					>
						Add provider
					</button>
				</div>
				<p class="field-hint">
					Nimbo talks to any OpenAI-compatible endpoint: OpenAI, Ollama, LM Studio, vLLM,
					OpenRouter. Provider API keys are encrypted at rest with the instance secret.
				</p>

				{#if providers.length === 0}
					<div class="empty-provider">
						<p class="field-hint">
							No providers configured yet. Add an OpenAI-compatible endpoint to get started.
						</p>
					</div>
				{/if}

				{#each providers as provider (provider.id)}
					<section class="provider-card">
						<div class="provider-head">
							<div class="provider-title">
								<strong>{providerLabel(provider)}</strong>
								<span>{provider.apiKeySet ? 'API key stored' : 'No API key stored'}</span>
							</div>
							<button class="btn danger" type="button" on:click={() => removeProvider(provider.id)}>
								Remove
							</button>
						</div>

						<div class="field">
							<label for={'provider-name-' + provider.id}>Provider name (optional)</label>
							<input
								id={'provider-name-' + provider.id}
								class="input"
								value={provider.name}
								on:input={(event) => updateProvider(provider.id, { name: inputValue(event) })}
								maxlength="100"
								placeholder="OpenAI, Local Ollama…"
							/>
						</div>

						<div class="field">
							<label for={'provider-base-' + provider.id}>Base URL</label>
							<input
								id={'provider-base-' + provider.id}
								class="input"
								value={provider.baseUrl}
								on:input={(event) => updateProvider(provider.id, { baseUrl: inputValue(event) })}
								placeholder="https://api.openai.com/v1"
							/>
						</div>

						<div class="field">
							<label for={'provider-key-' + provider.id}>API key (optional)</label>
							<input
								id={'provider-key-' + provider.id}
								class="input"
								type="password"
								value={provider.apiKey}
								on:input={(event) => updateProvider(provider.id, { apiKey: inputValue(event) })}
								disabled={provider.clearApiKey}
								placeholder={provider.apiKeySet
									? 'Stored key — leave blank to keep'
									: 'sk-… (optional)'}
							/>
							{#if provider.apiKeySet}
								<label class="checkbox-row">
									<input
										type="checkbox"
										checked={provider.clearApiKey}
										on:change={(event) =>
											updateProvider(provider.id, { clearApiKey: checkboxValue(event) })}
									/>
									Remove the stored API key when saving
								</label>
							{/if}
							{#if providerNeedsKeyConfirmation(provider)}
								<p class="field-hint warn-text">
									The provider origin changed. Saving will ask before reusing the key stored
									for the previous provider; otherwise enter a new key.
								</p>
							{/if}
						</div>

						<div class="model-toolbar">
							<button
								class="btn"
								type="button"
								on:click={() => fetchModels(provider.id)}
								disabled={provider.fetching || !provider.baseUrl.trim()}
							>
								{provider.fetching ? 'Fetching…' : 'Fetch models'}
							</button>
							{#if provider.fetchStatus}
								<span class="provider-status">{provider.fetchStatus}</span>
							{/if}
							{#if provider.fetchError}
								<span class="provider-error">{provider.fetchError}</span>
							{/if}
						</div>

						{#if provider.availableModels.length === 0}
							<p class="field-hint">
								Fetch models from this provider to choose which ones Nimbo may use. If the
								provider does not support the <code>/models</code> endpoint, add model IDs
								manually.
							</p>
						{/if}

						{#if provider.availableModels.length > 0}
							<div class="model-panel">
								<div class="model-toolbar">
									<input
										class="input model-filter"
										value={provider.modelFilter}
										on:input={(event) =>
											updateProvider(provider.id, { modelFilter: inputValue(event) })}
										placeholder="Filter models"
										aria-label="Filter models"
									/>
									<button class="btn ghost" type="button" on:click={() => selectAllModels(provider.id)}>
										Select all
									</button>
									<button class="btn ghost" type="button" on:click={() => clearModels(provider.id)}>
										Clear
									</button>
								</div>
								<p class="field-hint">
									{provider.selectedModels.length} of {provider.availableModels.length} selected
									for Nimbo.
								</p>
								<div class="model-list">
									{#each filteredModels(provider) as model (model)}
										<label class="model-row">
											<input
												type="checkbox"
												checked={provider.selectedModels.includes(model)}
												on:change={() => toggleModel(provider.id, model)}
											/>
											<code>{model}</code>
										</label>
									{/each}
									{#if filteredModels(provider).length === 0}
										<p class="field-hint">No models match that filter.</p>
									{/if}
								</div>
							</div>
						{/if}

						<div class="model-toolbar">
							<input
								class="input model-filter"
								value={provider.manualModel}
								on:input={(event) =>
									updateProvider(provider.id, { manualModel: inputValue(event) })}
								placeholder="Add a model ID manually"
								aria-label="Add a model ID manually"
							/>
							<button
								class="btn"
								type="button"
								on:click={() => addManualModel(provider.id)}
								disabled={!provider.manualModel.trim()}
							>
								Add model
							</button>
						</div>
					</section>
				{/each}

				<section class="role-section">
					<h2>Model roles</h2>
					<p class="field-hint">
						Choose from the models selected above. Each role can use a different provider and
						model.
					</p>
					<div class="role-grid">
						<div class="field">
							<label for="document-model">Document agent model</label>
							<select
								id="document-model"
								class="input"
								value={documentModelKey}
								on:change={onDocumentModelChange}
							>
								<option value="">Select a model</option>
								{#each modelOptions as option (modelRefKey(option))}
									<option value={modelRefKey(option)}>{option.label}</option>
								{/each}
							</select>
							<p class="field-hint">
								Used for vault chat and inline writing actions. Pick the strongest model you
								have.
							</p>
							{#if documentModel && !documentModelValid}
								<p class="field-hint warn-text">
									The current document agent model is no longer selected. Choose another
									model.
								</p>
							{/if}
						</div>
						<div class="field">
							<label for="indexing-model">File indexing model</label>
							<select
								id="indexing-model"
								class="input"
								value={indexingModelKey}
								on:change={onIndexingModelChange}
							>
								<option value="">Same as document agent</option>
								{#each modelOptions as option (modelRefKey(option))}
									<option value={modelRefKey(option)}>{option.label}</option>
								{/each}
							</select>
							<p class="field-hint">
								Used to describe uploaded images so they become searchable. Choose a fast,
								cheaper vision-capable model.
							</p>
							{#if indexingModel && !indexingModelValid}
								<p class="field-hint warn-text">
									The current file indexing model is no longer selected. Choose another model
									or use the document agent.
								</p>
							{/if}
						</div>
					</div>
				</section>

				<div class="field">
					<label for="temp">Document agent temperature ({temperature.toFixed(1)})</label>
					<input
						id="temp"
						type="range"
						min="0"
						max="2"
						step="0.1"
						bind:value={temperature}
						style="width: 100%;"
					/>
				</div>

				<div class="toggle-row">
					<label>
						<input type="checkbox" bind:checked={describeImages} />
						Describe uploaded images automatically
					</label>
					<p class="field-hint">
						Skipped images are requeued when you save. Image bytes are sent to the configured
						provider; descriptions stay private and are never exposed on public share links.
					</p>
				</div>

				<div class="actions">
					<button class="btn primary" type="button" on:click={save} disabled={!canSave}>
						{saving ? 'Saving…' : 'Save settings'}
					</button>
					<button class="btn" type="button" on:click={test} disabled={testing || !configured}>
						{testing ? 'Testing…' : 'Test saved document agent'}
					</button>
					<button class="btn ghost" type="button" on:click={() => goto('/vault')}>
						Back to vault
					</button>
				</div>

				<p class="footnote">Local models work without an API key. Leave it blank for Ollama.</p>
			</div>
		{:else}
			<div class="settings-panel" role="tabpanel" id="panel-tokens" aria-labelledby="tab-tokens">
				<div class="token-section tokens-tab">
					<div class="section-head">
						<h2>Device tokens</h2>
						<button class="btn primary" type="button" on:click={openQrModal}>Connect phone</button>
					</div>
					<p class="field-hint">
						Native clients, including the Android backup app, authenticate with a device token
						instead of a browser session. <strong>Backup-only</strong> tokens can reach only the
						<code>Phone Backups</code> folder and file endpoints. <strong>Full-account</strong>
						tokens grant the same access as your account, including documents and settings. Keep
						tokens private, prefer backup-only tokens for phones, and revoke ones you no longer
						use.
					</p>

					{#if tokenError}
						<div class="banner">{tokenError}</div>
					{/if}
					{#if tokenStatus}
						<div class="banner ok">{tokenStatus}</div>
					{/if}

					{#if newToken}
						<div class="token-reveal">
							<span>Copy this token now. It will not be shown again.</span>
							<code>{newToken}</code>
							<button class="btn ghost" type="button" on:click={copyToken}>Copy</button>
						</div>
					{/if}

					<form class="token-create" on:submit={createToken}>
						<input
							class="input"
							bind:value={tokenName}
							placeholder="Phone name, e.g. Pixel 9"
							maxlength="100"
							aria-label="Device token name"
							required
						/>
						<label class="scope-select">
							<span>Scope</span>
							<select class="input" bind:value={tokenScope} aria-label="Token scope">
								<option value="backup">Backup only</option>
								<option value="full">Full account</option>
							</select>
						</label>
						<button class="btn primary" type="submit" disabled={tokenBusy || !tokenName.trim()}>
							{tokenBusy ? 'Working…' : 'Create token'}
						</button>
					</form>

					{#if tokens.length > 0}
						<ul class="token-list">
							{#each tokens as token (token.id)}
								<li class="token-row">
									<div class="token-meta">
										<strong>{token.name}</strong>
										<span>
											Created {formatWhen(token.created_at)} · Last used {formatWhen(
												token.last_used_at
											)}
										</span>
										<span class="token-scope">{scopeLabel(token)}</span>
									</div>
									<span class="token-state">{token.status}</span>
									<button
										class="btn ghost"
										type="button"
										disabled={tokenBusy}
										on:click={() => revokeToken(token)}
									>
										Revoke
									</button>
								</li>
							{/each}
						</ul>
					{:else}
						<p class="field-hint">No device tokens yet.</p>
					{/if}
				</div>
			</div>
		{/if}
	</div>
</div>

{#if qrOpen}
	<div class="qr-overlay">
		<div class="qr-card">
			{#if qrStep === 'name'}
				<h2>Connect a phone</h2>
				<p class="field-hint">
					Creates a one-time device token and shows it as a QR code. In the Nimbo Backup app on
					your phone, tap “Scan QR code” and point the camera here.
				</p>
				{#if qrError}
					<div class="banner">{qrError}</div>
				{/if}
				<div class="field">
					<label for="qr-name">Device name</label>
					<input id="qr-name" class="input" bind:value={qrName} maxlength="100" />
				</div>
				<div class="qr-actions">
					<button
						class="btn primary"
						type="button"
						disabled={qrBusy || !qrName.trim()}
						on:click={generateQr}
					>
						{qrBusy ? 'Working…' : 'Show QR code'}
					</button>
					<button class="btn ghost" type="button" on:click={closeQrModal}>Cancel</button>
				</div>
			{:else}
				<h2>Scan with your phone</h2>
				<canvas class="qr-canvas" bind:this={qrCanvas}></canvas>
				<p class="field-hint">
					Open the Nimbo Backup app and tap “Scan QR code”. This token is shown only once — if
					you stop here, revoke it so it cannot be reused.
				</p>
				<div class="qr-actions">
					<button class="btn ghost" type="button" disabled={qrBusy} on:click={revokeQrToken}>
						Revoke &amp; close
					</button>
					<button class="btn" type="button" on:click={closeQrModal}>Done</button>
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.settings-shell {
		min-height: 100vh;
		display: grid;
		place-items: start center;
		padding: 32px 20px;
	}
	.settings-card {
		width: 100%;
		max-width: 980px;
		background: linear-gradient(180deg, var(--panel-2), var(--panel));
		border: 1px solid var(--line);
		border-radius: 18px;
		padding: 28px;
		box-shadow: 0 30px 80px rgba(0, 0, 0, 0.55);
	}
	.settings-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
	}
	.settings-head h1 {
		margin: 18px 0 6px;
		font-size: 24px;
		letter-spacing: -0.02em;
	}
	.settings-head p.sub {
		margin: 0 0 22px;
		color: var(--muted);
		font-size: 14px;
	}
	.settings-tabs {
		display: flex;
		gap: 4px;
		margin: 22px 0 18px;
		border-bottom: 1px solid var(--line-soft);
	}
	.settings-tab {
		background: transparent;
		border: 0;
		border-bottom: 2px solid transparent;
		color: var(--muted);
		padding: 10px 12px;
		font-weight: 600;
		cursor: pointer;
	}
	.settings-tab:hover {
		color: var(--text);
	}
	.settings-tab.active {
		color: var(--text);
		border-bottom-color: var(--accent);
	}
	.settings-panel {
		display: grid;
		gap: 16px;
	}
	.provider-card {
		display: grid;
		gap: 12px;
		padding: 16px;
		border: 1px solid var(--line-soft);
		border-radius: 12px;
		background: var(--bg-2);
	}
	.provider-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.provider-title {
		display: grid;
		gap: 3px;
		min-width: 0;
	}
	.provider-title strong {
		font-size: 14px;
	}
	.provider-title span {
		color: var(--faint);
		font-size: 11.5px;
	}
	.model-toolbar {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
	}
	.model-filter {
		flex: 1;
		min-width: 160px;
	}
	.model-panel {
		display: grid;
		gap: 10px;
		padding: 12px;
		border: 1px solid var(--line-soft);
		border-radius: 10px;
		background: var(--bg);
	}
	.model-list {
		display: grid;
		gap: 4px;
		max-height: 220px;
		overflow: auto;
		padding: 8px;
		border: 1px solid var(--line-soft);
		border-radius: 8px;
		background: var(--bg-2);
	}
	.model-row {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 12px;
		cursor: pointer;
	}
	.model-row input[type='checkbox'] {
		width: 15px;
		height: 15px;
		accent-color: var(--accent);
	}
	.model-row code {
		font-size: 12px;
		overflow-wrap: anywhere;
	}
	.provider-status {
		color: var(--good);
		font-size: 12px;
	}
	.provider-error {
		color: #ffc2cc;
		font-size: 12px;
	}
	.empty-provider {
		padding: 14px;
		border: 1px dashed var(--line);
		border-radius: 10px;
	}
	.role-section {
		padding-top: 16px;
		border-top: 1px solid var(--line-soft);
	}
	.role-section h2 {
		margin: 0 0 6px;
		font-size: 15px;
	}
	.role-grid {
		display: grid;
		gap: 12px;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
	}
	.actions {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
		margin-top: 4px;
	}
	.section-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.section-head h2 {
		margin: 0;
		font-size: 15px;
	}
	.field-hint {
		margin: 6px 0 0;
		font-size: 11.5px;
		line-height: 1.5;
		color: var(--faint);
	}
	.warn-text {
		color: #ffd59a;
	}
	.checkbox-row {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-top: 8px;
		font-size: 12px;
		color: var(--muted);
		cursor: pointer;
	}
	.checkbox-row input[type='checkbox'] {
		width: 15px;
		height: 15px;
		accent-color: var(--accent);
	}
	.toggle-row {
		margin: 4px 0 16px;
		padding: 10px 12px;
		border: 1px solid var(--line-soft);
		border-radius: 10px;
		background: var(--bg-2);
	}
	.toggle-row label {
		display: flex;
		align-items: center;
		gap: 9px;
		font-size: 13px;
		cursor: pointer;
	}
	.toggle-row input[type='checkbox'] {
		width: 16px;
		height: 16px;
		accent-color: var(--accent);
	}

	.token-section {
		margin-top: 22px;
		padding-top: 20px;
		border-top: 1px solid var(--line-soft);
	}
	.tokens-tab {
		margin-top: 0;
		padding-top: 0;
		border-top: 0;
	}
	.token-section h2 {
		margin: 0 0 6px;
		font-size: 15px;
	}
	.token-create {
		display: flex;
		gap: 8px;
		margin: 12px 0;
		flex-wrap: wrap;
		align-items: flex-end;
	}
	.token-create .input {
		flex: 1;
		min-width: 160px;
	}
	.scope-select {
		display: grid;
		gap: 4px;
		font-size: 10.5px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--faint);
	}
	.scope-select .input {
		min-width: 150px;
		padding: 9px 10px;
	}
	.token-scope {
		color: var(--muted) !important;
		font-size: 11px;
	}
	.token-reveal {
		display: grid;
		gap: 8px;
		margin: 12px 0;
		padding: 12px;
		border: 1px solid var(--line-soft);
		border-radius: 10px;
		background: var(--bg-2);
		font-size: 12px;
	}
	.token-reveal code {
		display: block;
		overflow-wrap: anywhere;
		padding: 8px;
		border-radius: 6px;
		background: var(--bg);
		font-size: 12px;
	}
	.token-reveal .btn {
		justify-self: start;
	}
	.token-list {
		display: grid;
		gap: 8px;
		margin: 12px 0 0;
		padding: 0;
		list-style: none;
	}
	.token-row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 10px 12px;
		border: 1px solid var(--line-soft);
		border-radius: 10px;
		background: var(--bg-2);
	}
	.token-meta {
		display: grid;
		gap: 3px;
		flex: 1;
		min-width: 0;
		font-size: 12px;
	}
	.token-meta strong {
		font-size: 13px;
	}
	.token-meta span {
		color: var(--faint);
	}
	.token-state {
		padding: 2px 7px;
		border-radius: 999px;
		background: var(--bg);
		color: var(--faint);
		font-size: 11px;
		text-transform: capitalize;
	}
	.qr-overlay {
		position: fixed;
		inset: 0;
		z-index: 50;
		display: grid;
		place-items: center;
		padding: 20px;
		background: rgba(0, 0, 0, 0.55);
	}
	.qr-card {
		display: grid;
		gap: 12px;
		width: min(420px, 100%);
		padding: 20px;
		border: 1px solid var(--line-soft);
		border-radius: 14px;
		background: var(--bg-2, #fff);
		box-shadow: 0 18px 50px rgba(0, 0, 0, 0.35);
	}
	.qr-card h2 {
		margin: 0;
		font-size: 16px;
	}
	.qr-canvas {
		justify-self: center;
		width: min(280px, 100%);
		height: auto;
		border-radius: 8px;
	}
	.qr-actions {
		display: flex;
		gap: 8px;
	}
	@media (max-width: 640px) {
		.settings-card {
			padding: 20px;
		}
		.settings-head {
			flex-direction: column;
		}
		.role-grid {
			grid-template-columns: 1fr;
		}
		.provider-head {
			align-items: flex-start;
			flex-direction: column;
		}
	}
</style>
