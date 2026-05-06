<script lang="ts">
	import { onMount } from 'svelte';
	import { listJobRuns, type JobRun } from '$lib/api';

	let runs = $state<JobRun[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			runs = await listJobRuns();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	function formatBytes(n: number | null): string {
		if (n === null || n === undefined) return '—';
		if (n < 1024) return `${n} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let v = n / 1024;
		let unit = 0;
		while (v >= 1024 && unit < units.length - 1) {
			v /= 1024;
			unit++;
		}
		return `${v.toFixed(1)} ${units[unit]}`;
	}

	function formatTime(iso: string | null): string {
		if (!iso) return '—';
		// Strip the optional RFC 9557 IANA-tz annotation that rustic_core
		// emits (`...+02:00[+02:00]`) before handing to the Date parser.
		const cleaned = iso.replace(/\[[^\]]+\]$/, '');
		const d = new Date(cleaned);
		if (isNaN(d.getTime())) return iso;
		return d.toLocaleString();
	}
</script>

<h2>Runs</h2>

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else if runs.length === 0}
	<p class="empty">No runs recorded yet. Trigger one from the CLI or via <code>POST /api/jobs/&lt;name&gt;/run</code>.</p>
{:else}
	<table>
		<thead>
			<tr>
				<th>Job</th>
				<th>Started</th>
				<th>Finished</th>
				<th>Status</th>
				<th>Bytes</th>
				<th>Trigger</th>
				<th>Run ID</th>
			</tr>
		</thead>
		<tbody>
			{#each runs as run (run.id)}
				<tr class={`row-${run.status}`}>
					<td>{run.job_name}</td>
					<td>{formatTime(run.started_at)}</td>
					<td>{formatTime(run.finished_at)}</td>
					<td>{run.status}</td>
					<td>{formatBytes(run.bytes_processed)}</td>
					<td>{run.trigger}</td>
					<td class="mono">{run.id.slice(0, 8)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

<style>
	h2 {
		margin: 0 0 1.25rem;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
	}

	thead th {
		text-align: left;
		padding: 0.5rem 0.75rem;
		border-bottom: 1px solid #2a2f38;
		font-weight: 500;
		color: #9aa3b2;
	}

	tbody td {
		padding: 0.5rem 0.75rem;
		border-bottom: 1px solid #1a1d24;
	}

	tbody tr:hover td {
		background: #161a21;
	}

	.row-running td {
		color: #f5d36a;
	}
	.row-failed td {
		color: #f47373;
	}

	.mono {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
	}

	.error {
		color: #f47373;
	}

	.empty {
		color: #9aa3b2;
		font-size: 0.95rem;
	}
</style>
