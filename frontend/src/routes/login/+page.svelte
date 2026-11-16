<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { api } from '$lib/api';

	let instanceName = 'Nimbo';
	let username = '';
	let password = '';
	let error = '';
	let busy = false;

	onMount(async () => {
		try {
			const boot = await api.bootstrap();
			if (!boot.setup_complete) {
				goto('/setup');
				return;
			}
			if (boot.user) {
				goto('/vault');
				return;
			}
			instanceName = boot.instance_name || 'Nimbo';
		} catch {
			/* ignore */
		}
	});

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = '';
		busy = true;
		try {
			await api.login({ username, password });
			goto('/vault');
		} catch (err) {
			error = err instanceof Error ? err.message : 'Login failed';
		} finally {
			busy = false;
		}
	}
</script>

<div class="auth-shell">
	<div class="auth-card">
		<div class="brand">
			<span class="mark">N</span>
			<span class="name">{instanceName}</span>
		</div>
		<h1>Welcome back</h1>
		<p class="sub">Sign in to your workspace.</p>

		{#if error}
			<div class="banner">{error}</div>
		{/if}

		<form on:submit={submit}>
			<div class="field">
				<label for="username">Username</label>
				<input
					id="username"
					class="input"
					bind:value={username}
					autocomplete="username"
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
					autocomplete="current-password"
					required
				/>
			</div>
			<button class="btn primary block" type="submit" disabled={busy}>
				{busy ? 'Signing in…' : 'Sign in'}
			</button>
		</form>

		<p class="footnote">Sessions are HttpOnly and SameSite=Strict.</p>
	</div>
</div>
