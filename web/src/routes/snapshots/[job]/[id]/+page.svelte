<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { listSnapshots, type Snapshot } from '$lib/api';
	import { formatBytes, formatTime } from '$lib/format';

	const jobName = $derived(page.params.job);
	const snapId = $derived(page.params.id);

	let snapshot = $state<Snapshot | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const all = await listSnapshots();
			snapshot = all.find((s) => s.id === snapId && s.job_name === jobName) ?? null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});
</script>

<h2>snapshot {snapId}</h2>

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else if !snapshot}
	<p class="error">No snapshot <code>{snapId}</code> for job <code>{jobName}</code>.</p>
{:else}
	<dl>
		<dt>Job</dt>
		<dd><a href="/jobs/{snapshot.job_name}">{snapshot.job_name}</a></dd>
		<dt>Time</dt>
		<dd>{formatTime(snapshot.time)}</dd>
		<dt>Hostname</dt>
		<dd>{snapshot.hostname}</dd>
		<dt>Bytes</dt>
		<dd>{formatBytes(snapshot.bytes_total)}</dd>
		<dt>Paths</dt>
		<dd>
			<ul class="paths">
				{#each snapshot.paths as p}
					<li class="mono">{p}</li>
				{/each}
			</ul>
		</dd>
		<dt>ID</dt>
		<dd class="mono">{snapshot.id}</dd>
	</dl>

	<p class="hint">
		Restore via <code>rustic</code> CLI:
		<code class="block">rustic restore {snapshot.id}:/ &lt;destination&gt;</code>
	</p>
{/if}

<style>
	h2 {
		margin: 0 0 1.25rem;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}

	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.4rem 1rem;
		padding: 0.75rem 1rem;
		background: #161a21;
		border-radius: 4px;
	}
	dt {
		color: #9aa3b2;
		font-size: 0.85rem;
	}
	dd {
		margin: 0;
		font-size: 0.9rem;
	}

	.paths {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	.paths li {
		padding: 0.1rem 0;
	}

	.mono {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
	}

	.hint {
		margin-top: 1.25rem;
		color: #9aa3b2;
		font-size: 0.9rem;
	}
	.block {
		display: block;
		padding: 0.5rem 0.75rem;
		margin-top: 0.4rem;
		background: #161a21;
		border-radius: 4px;
	}

	a {
		color: #80a8e6;
		text-decoration: none;
	}
	a:hover {
		text-decoration: underline;
	}

	.error {
		color: #f47373;
	}
</style>
