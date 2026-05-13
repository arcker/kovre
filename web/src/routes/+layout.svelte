<script lang="ts">
	let { children } = $props();

	let refreshing = $state(false);
	let refreshMessage = $state<string | null>(null);

	async function refresh() {
		refreshing = true;
		refreshMessage = null;
		try {
			const resp = await fetch('/api/sync', { method: 'POST' });
			if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
			const body = await resp.json();
			refreshMessage = `synced ${body.synced ?? 0} snapshot(s)`;
			// Reload the current page to pull the fresh projection into
			// every route's local state. Cheap and predictable; we'll
			// switch to a Svelte store if more places need to react to
			// sync events.
			setTimeout(() => location.reload(), 400);
		} catch (e) {
			refreshMessage = e instanceof Error ? e.message : String(e);
		} finally {
			refreshing = false;
		}
	}
</script>

<header>
	<h1>kovre</h1>
	<nav>
		<a href="/">overview</a>
		<a href="/runs">runs</a>
		<a href="/templates">+ add job</a>
		<a href="/repositories">repositories</a>
		<a href="/about">about</a>
	</nav>
	<div class="actions">
		{#if refreshMessage}
			<span class="msg">{refreshMessage}</span>
		{/if}
		<button type="button" class="refresh" disabled={refreshing} onclick={refresh}>
			{refreshing ? '↻ syncing…' : '↻ Refresh'}
		</button>
	</div>
</header>

<main>
	{@render children()}
</main>

<style>
	:global(body) {
		font-family: ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif;
		margin: 0;
		padding: 0;
		background: #0f1115;
		color: #e6e8eb;
	}

	header {
		display: flex;
		align-items: center;
		gap: 2rem;
		padding: 1rem 2rem;
		border-bottom: 1px solid #2a2f38;
	}

	header h1 {
		margin: 0;
		font-size: 1.25rem;
		font-weight: 600;
		letter-spacing: 0.02em;
	}

	header nav {
		display: flex;
		gap: 1.25rem;
	}

	header nav a {
		color: #9aa3b2;
		text-decoration: none;
		font-size: 0.95rem;
	}

	header nav a:hover {
		color: #e6e8eb;
	}

	header .actions {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 1rem;
	}

	header .msg {
		color: #6ad08e;
		font-size: 0.85rem;
	}

	header .refresh {
		padding: 0.35rem 0.75rem;
		background: #1f242c;
		color: #c5cad3;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		cursor: pointer;
		font: inherit;
		font-size: 0.85rem;
	}
	header .refresh:hover:not(:disabled) {
		background: #262c36;
		color: #e6e8eb;
	}
	header .refresh:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	main {
		padding: 2rem;
		max-width: 1200px;
		margin: 0 auto;
	}
</style>
