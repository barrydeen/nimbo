<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { api, type DeviceToken, type LlmSettings } from '$lib/api';

	let baseUrl = 'https://api.openai.com/v1';
	let model = '';
	let apiKey = '';
	let temperature = 0.7;
	let visionModel = '';
	let describeImages = true;
	let apiKeySet = false;
	let configured = false;
	let error = '';
	let status = '';
	let busy = false;
	let testing = false;

	let tokens: DeviceToken[] = [];
	let tokenName = '';
	let newToken = '';
	let tokenError = '';
	let tokenStatus = '';
	let tokenBusy = false;

	function formatWhen(ts: number | null) {
		return ts ? new Date(ts * 1000).toLocaleString() : 'Never';
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
			const result = await api.createToken(tokenName.trim());
			newToken = result.token;
			tokenName = '';
			await loadTokens();
			tokenStatus = 'Token created. Copy it now — it is shown only once.';
		} catch (err) {
			tokenError = err instanceof Error ? err.message : 'Could not create token';
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
			tokenError = err instanceof Error ? err.message : 'Could not revoke token';
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

	onMount(async () => {
		try {
			const boot = await api.bootstrap();
			if (!boot.setup_complete) {
				goto('/setup');
				return;
			}
			if (!boot.user) {
				goto('/login');
				return;
			}
			if (boot.user.role !== 'admin') {
				goto('/vault');
				return;
			}
			const settings = await api.getLlm();
			baseUrl = settings.base_url;
			model = settings.model;
			temperature = settings.temperature;
			visionModel = settings.vision_model ?? '';
			describeImages = settings.describe_images ?? true;
			apiKeySet = settings.api_key_set;
			configured = settings.configured;
			await loadTokens();
		} catch (err) {
			error = err instanceof Error ? err.message : 'Could not load settings';
		}
	});

	async function save(event: SubmitEvent) {
		event.preventDefault();
		error = '';
		status = '';
		busy = true;
		try {
			const saved = await api.saveLlm({
				base_url: baseUrl,
				model,
				api_key: apiKey || undefined,
				temperature,
				vision_model: visionModel,
				describe_images: describeImages
			});
			apiKeySet = saved.api_key_set;
			configured = saved.configured;
			visionModel = saved.vision_model ?? '';
			describeImages = saved.describe_images ?? true;
			apiKey = '';
			status = 'Saved. Agent features and image descriptions are now available.';
		} catch (err) {
			error = err instanceof Error ? err.message : 'Save failed';
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
			error = err instanceof Error ? err.message : 'Test failed';
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
					chat model when it supports images.
				</p>
			</div>
			<div class="field">
				<label for="key">API key</label>
				<input
					id="key"
					class="input"
					type="password"
					bind:value={apiKey}
					placeholder={apiKeySet ? 'Stored key — leave blank to keep' : 'sk-…'}
				/>
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
			<h2>Device tokens</h2>
			<p class="field-hint">
				Native clients, including the Android backup app, authenticate with a device token
				instead of a browser session. Tokens grant the same access as your account, so keep
				them private and revoke ones you no longer use.
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

<style>
	.field-hint {
		margin: 6px 0 0;
		font-size: 11.5px;
		line-height: 1.5;
		color: var(--faint);
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
	}
	.token-create .input {
		flex: 1;
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
</style>
