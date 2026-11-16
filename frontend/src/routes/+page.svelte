<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { api } from '$lib/api';

	onMount(async () => {
		try {
			const boot = await api.bootstrap();
			if (!boot.setup_complete) {
				goto('/setup');
			} else if (!boot.user) {
				goto('/login');
			} else {
				goto('/vault');
			}
		} catch {
			goto('/login');
		}
	});
</script>

<div class="center">
	<div class="spinner"></div>
</div>
