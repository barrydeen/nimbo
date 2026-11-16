<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { api } from '$lib/api';

	let instanceName = 'Nimbo';
	let username = '';
	let password = '';
	let confirm = '';
	let setupToken = '';
	let setupTokenRequired = false;
	let error = '';
	let busy = false;

	onMount(async () => {
		try {
			const boot = await api.bootstrap();
			if (boot.setup_complete) {
				goto('/login');
				return;
			}
			instanceName = boot.instance_name || 'Nimbo';
			setupTokenRequired = boot.setup_token_required === true;
		} catch {
			/* first boot, backend may still be starting */
		}
	});

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = '';
		if (password.length < 8) {
			error = 'Password must be at least 8 characters.';
			return;
		}
		if (password !== confirm) {
			error = 'Passwords do not match.';
			return;
		}
		if (setupTokenRequired && !setupToken.trim()) {
			error = 'This server requires the setup token configured by its administrator.';
			return;
		}
		busy = true;
		try {
			await api.setup({
				instance_name: instanceName,
				username,
				password,
				...(setupToken.trim() ? { setup_token: setupToken.trim() } : {})
			});
			goto('/vault');
		} catch (err) {
			error = err instanceof Error ? err.message : 'Setup failed';
		} finally {
			busy = false;
		}
	}
</script>

<div class="auth-shell">
	<div class="auth-card">
		<div class="brand">
			<span class="mark">N</span>
			<span class="name">Nimbo</span>
		</div>
		<h1>Set up your instance</h1>
		<p class="sub">
			Create the administrator account. This is a fresh instance, so nothing exists yet.
		</p>

		{#if error}
			<div class="banner">{error}</div>
		{/if}

		<form on:submit={submit}>
			<div class="field">
				<label for="instance">Instance name</label>
				<input
					id="instance"
					class="input"
					bind:value={instanceName}
					maxlength="64"
					required
				/>
			</div>
			{#if setupTokenRequired}
				<div class="field">
					<label for="setup-token">Setup token</label>
					<input
						id="setup-token"
						class="input"
						bind:value={setupToken}
						autocomplete="off"
						required
					/>
					<p class="field-hint">
						This server was started with a bootstrap token. Enter the value from its
						<code>NIMBO_SETUP_TOKEN</code> environment setting.
					</p>
				</div>
			{/if}
			<div class="field">
				<label for="username">Admin username</label>
				<input
					id="username"
					class="input"
					bind:value={username}
					autocomplete="username"
					minlength="3"
					maxlength="32"
					required
				/>
			</div>
			<div class="field">
				<label for="password">Password</label>
				<input
					id="password"
					class="input"
					type="password"
					bind:value={password}
					autocomplete="new-password"
					minlength="8"
					required
				/>
			</div>
			<div class="field">
				<label for="confirm">Confirm password</label>
				<input
					id="confirm"
					class="input"
					type="password"
					bind:value={confirm}
					autocomplete="new-password"
					required
				/>
			</div>
			<button class="btn primary block" type="submit" disabled={busy}>
				{busy ? 'Creating instance…' : 'Create instance'}
			</button>
		</form>

		<p class="footnote">Markdown vault · full-text search · agent-ready</p>
	</div>
</div>
