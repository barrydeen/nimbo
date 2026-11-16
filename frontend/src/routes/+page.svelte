<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { api, errorMessage, isUnauthorized } from '$lib/api';

	let error = '';
	let busy = false;

	async function route() {
		busy = true;
		error = '';
		try {
			const boot = await api.bootstrap();
			if (!boot.setup_complete) {
				await goto('/setup');
			} else if (!boot.user) {
				await goto('/login');
			} else {
				await goto('/vault');
			}
		} catch (err) {
			if (isUnauthorized(err)) {
				await goto('/login');
				return;
			}
			error = `Nimbo could not reach its server: ${errorMessage(err)}`;
		} finally {
			busy = false;
		}
	}

	onMount(() => {
		void route();
	});
</script>

<div class="center">
	{#if error}
		<div class="empty-state">
			<div class="inner">
				<h2>Nimbo is unavailable</h2>
				<p>{error}</p>
				<button class="btn primary" on:click={route} disabled={busy}>
					{busy ? 'Retrying…' : 'Retry'}
				</button>
			</div>
		</div>
	{:else}
		<div class="spinner"></div>
	{/if}
</div>
