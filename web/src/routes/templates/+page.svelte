<script lang="ts">
	import { onMount } from 'svelte';
	import { listTemplates, type Template } from '$lib/api';

	let templates = $state<Template[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			templates = await listTemplates();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});
</script>

<h2>Add a job from a template</h2>
<p class="lead">
	Pick a template to declare a new backup job in <code>kovre.yaml</code>. The
	dashboard writes the file for you and reloads the running config — no restart.
</p>

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else}
	<div class="grid">
		{#each templates as t (t.name)}
			<a class="card" href={`/templates/${t.name}`}>
				<span class="icon">{t.icon}</span>
				<h3>{t.name}</h3>
				<p>{t.description}</p>
			</a>
		{/each}
	</div>
{/if}

<style>
	h2 {
		margin: 0 0 0.5rem;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}
	.lead {
		color: #9aa3b2;
		font-size: 0.95rem;
		max-width: 640px;
		margin: 0 0 1.5rem;
	}
	.lead code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
		color: #c5cad3;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
		gap: 1rem;
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 1.5rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 8px;
		text-decoration: none;
		color: inherit;
		transition: border-color 0.1s;
	}
	.card:hover {
		border-color: #355fb0;
	}
	.icon {
		font-size: 2.2rem;
		line-height: 1;
	}
	.card h3 {
		margin: 0;
		font-size: 1.1rem;
		font-weight: 600;
		color: #e6e8eb;
	}
	.card p {
		margin: 0;
		font-size: 0.9rem;
		color: #9aa3b2;
	}

	.error {
		color: #f47373;
	}
</style>
