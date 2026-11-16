<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import qrcode from 'qrcode-generator';
	import {
		api,
		ApiError,
		errorMessage,
		isUnauthorized,
		type DeviceToken,
		type LlmSettings,
		type TokenScope
	} from '$lib/api';

	let baseUrl = 'https://api.openai.com/v1';
	let model = '';
	let apiKey = '';
	let temperature = 0.7;
	let visionModel = '';
	let describeImages = true;
	let apiKeySet = false;
	let configured = false;
	let clearApiKey = false;
	let keyOrigin = '';
	let error = '';
	let status = '';
	let busy = false;
	let testing = false;

	let tokens: DeviceToken[] = [];
	let tokenName = '';
	let tokenScope: TokenScope = 'backup';
	let newToken = '';
	let tokenError = '';
	let tokenStatus = '';
	let tokenBusy = false;
	let loading = false;

	function formatWhen(ts: number | null) {
		return ts ? new Date(ts * 1000).toLocaleString() : 'Never';
	}

	function originOf(value: string): string {
		try {
			return new URL(value).origin;
		} catch {
			return '';
		}
	}

	function scopeLabel(token: DeviceToken): string {
		return token.scope === 'backup' ? 'Backup only · Phone Backups' : 'Full account';
	}

	function message(err: unknown): string {
		return errorMessage(err);
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

	let qrOpen = false;
	let qrStep: 'name' | 'qr' = 'name';
	let qrName = 'My phone';
	let qrBusy = false;
	let qrError = '';
	let qrPayload = '';
	let qrTokenId = '';
	let qrCanvas: HTMLCanvasElement | undefined;

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

	function applySettings(settings: LlmSettings) {
		baseUrl = settings.base_url;
		model = settings.model;
		temperature = settings.temperature;
		visionModel = settings.vision_model ?? '';
		describeImages = settings.describe_images ?? true;
		apiKeySet = settings.api_key_set;
		configured = settings.configured;
		keyOrigin = originOf(settings.base_url);
	}

	function saveBody(confirmReuse: boolean) {
		const vision = visionModel.trim();
		return {
			base_url: baseUrl,
			model,
			temperature,
			// An explicitly empty vision model clears the override; a non-empty
			// value replaces it.
			vision_model: vision,
			describe_images: describeImages,
			...(apiKey.trim() && !clearApiKey ? { api_key: apiKey.trim() } : {}),
			...(clearApiKey ? { clear_api_key: true } : {}),
			...(confirmReuse ? { confirm_reuse_api_key: true } : {})
		};
	}

	async function save(event: SubmitEvent) {
		event.preventDefault();
		error = '';
		status = '';
		busy = true;
		try {
			const reusingKey = apiKeySet && !apiKey.trim() && !clearApiKey;
			const changingOrigin = reusingKey && !!keyOrigin && originOf(baseUrl) !== keyOrigin;
			if (changingOrigin) {
				const confirmed = confirm(
					'The provider origin changed. Reuse the API key stored for the previous provider with this new origin? Cancel to keep the old key unused and enter a new one.'
				);
				if (!confirmed) {
					error =
						'Not saved: confirm key reuse or enter a new API key. The stored key was not sent to the new provider.';
					return;
				}
			}
			const saved = await api.saveLlm(saveBody(changingOrigin));
			const wantedVision = visionModel.trim();
			applySettings(saved);
			apiKey = '';
			clearApiKey = false;
			status =
				wantedVision === '' && !saved.vision_model
					? 'Saved. No dedicated vision model is configured; the chat model will be used.'
					: 'Saved. Agent features and image descriptions are now available.';
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			// The backend requires an explicit confirmation when the stored key
			// would be reused for a different origin (409).
			if (err instanceof ApiError && err.status === 409) {
				const confirmed = confirm(
					`The server requires confirmation to reuse the stored API key for this provider origin.\n\n${err.message}\n\nReuse the stored key?`
				);
				if (confirmed) {
					try {
						const saved = await api.saveLlm(saveBody(true));
						const wantedVision = visionModel.trim();
						applySettings(saved);
						apiKey = '';
						clearApiKey = false;
						status =
							wantedVision === '' && !saved.vision_model
								? 'Saved. No dedicated vision model is configured; the chat model will be used.'
								: 'Saved.';
						return;
					} catch (retryErr) {
						if (isUnauthorized(retryErr)) {
							await goto('/login');
							return;
						}
						error = message(retryErr);
						return;
					}
				}
			}
			error = message(err);
		} finally {
			busy = false;
		}
	}

	async function test() {
		error = '';
		status = '';
		testing = true;
		try {
			const result = await api.testLlm();
			status = `Provider responded: ${result.reply.trim()}`;
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
</script>

<div class="auth-shell">
	<div class="auth-card" style="max-width: 560px;">
		<div class="brand">
			<span class="mark">N</span>
			<span class="name">Agent settings</span>
		</div>
		<h1>Connect a model</h1>
		<p class="sub">
			Nimbo talks to any OpenAI-compatible endpoint: OpenAI, Ollama, LM Studio, vLLM,
			OpenRouter. The API key is encrypted at rest with the instance secret.
		</p>

		{#if error}
			<div class="banner">{error}</div>
		{/if}
		{#if status}
			<div class="banner ok">{status}</div>
		{/if}

		<form on:submit={save}>
			<div class="field">
				<label for="base">Base URL</label>
				<input id="base" class="input" bind:value={baseUrl} required />
			</div>
			<div class="field">
				<label for="model">Model</label>
				<input id="model" class="input" bind:value={model} placeholder="gpt-4o-mini" required />
			</div>
			<div class="field">
				<label for="vision">Vision model (optional)</label>
				<input
					id="vision"
					class="input"
					bind:value={visionModel}
					placeholder={model || 'gpt-4o-mini, llava, qwen2.5-vl…'}
				/>
				<p class="field-hint">
					Used to describe uploaded images so they become searchable. Leave blank to use the
					chat model when it supports images. Clearing this field removes the dedicated vision
					model override and the chat model is used instead.
				</p>
			</div>
			<div class="field">
				<label for="key">API key</label>
				<input
					id="key"
					class="input"
					type="password"
					bind:value={apiKey}
					disabled={clearApiKey}
					placeholder={apiKeySet ? 'Stored key — leave blank to keep' : 'sk-…'}
				/>
				{#if apiKeySet}
					<label class="checkbox-row">
						<input type="checkbox" bind:checked={clearApiKey} />
						Remove the stored API key when saving
					</label>
				{/if}
				{#if apiKeySet && !apiKey.trim() && !clearApiKey && keyOrigin && originOf(baseUrl) !== keyOrigin}
					<p class="field-hint warn-text">
						The provider origin changed. Saving will ask before reusing the key stored for the
						previous provider; otherwise enter a new key.
					</p>
				{/if}
			</div>
			<div class="field">
				<label for="temp">Temperature ({temperature.toFixed(1)})</label>
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
					Skipped images are requeued when you save. Image bytes are sent to your configured
					provider; descriptions stay private and are never exposed on public share links.
				</p>
			</div>

			<div style="display: flex; gap: 8px; margin-top: 6px;">
				<button class="btn primary" type="submit" disabled={busy}>
					{busy ? 'Saving…' : 'Save'}
				</button>
				<button class="btn" type="button" on:click={test} disabled={testing || !configured}>
					{testing ? 'Testing…' : 'Test connection'}
				</button>
				<button class="btn ghost" type="button" on:click={() => goto('/vault')}>
					Back to vault
				</button>
			</div>
		</form>

		<section class="token-section">
			<div class="section-head">
				<h2>Device tokens</h2>
				<button class="btn primary" type="button" on:click={openQrModal}>Connect phone</button>
			</div>
			<p class="field-hint">
				Native clients, including the Android backup app, authenticate with a device token
				instead of a browser session. <strong>Backup-only</strong> tokens can reach only the
				<code>Phone Backups</code> folder and file endpoints. <strong>Full-account</strong> tokens
				grant the same access as your account, including documents and settings. Keep tokens
				private, prefer backup-only tokens for phones, and revoke ones you no longer use.
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
		</section>

		<p class="footnote">
			Local models work without an API key. Leave it blank for Ollama.
		</p>
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
	.section-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.section-head h2 {
		margin: 0;
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
</style>
