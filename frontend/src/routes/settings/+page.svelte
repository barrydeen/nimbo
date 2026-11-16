<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { api, type LlmSettings } from '$lib/api';

	let baseUrl = 'https://api.openai.com/v1';
	let model = '';
	let apiKey = '';
	let temperature = 0.7;
	let apiKeySet = false;
	let configured = false;
	let error = '';
	let status = '';
	let busy = false;
	let testing = false;

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
			apiKeySet = settings.api_key_set;
			configured = settings.configured;
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
				temperature
			});
			apiKeySet = saved.api_key_set;
			configured = saved.configured;
			apiKey = '';
			status = 'Saved. Agent features are now available.';
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

		<p class="footnote">
			Local models work without an API key. Leave it blank for Ollama.
		</p>
	</div>
</div>
