<script lang="ts">
	import { onMount } from 'svelte';
	import { listJobRuns, type JobRun } from '$lib/api';
	import { formatBytes, formatTime } from '$lib/format';
	import init, { sortRunsBy } from 'kovre-wasm';

	type SortDir = 'asc' | 'desc';

	const COLUMNS: Array<{ key: keyof JobRun; label: string }> = [
		{ key: 'job_name', label: 'Job' },
		{ key: 'started_at', label: 'Started' },
		{ key: 'finished_at', label: 'Finished' },
		{ key: 'status', label: 'Status' },
		{ key: 'bytes_processed', label: 'Bytes' },
		{ key: 'trigger', label: 'Trigger' },
		{ key: 'id', label: 'Run ID' }
	];

	let runs = $state<JobRun[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let sortKey = $state<keyof JobRun>('started_at');
	let sortDir = $state<SortDir>('desc');
	let wasmReady = $state(false);

	onMount(async () => {
		// Load and initialize the WASM bundle before fetching, so the very
		// first render is already sorted via WASM (no JS Array.sort fallback).
		await init();
		wasmReady = true;
		try {
			const data = await listJobRuns();
			runs = applySort(data, sortKey, sortDir);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	// Phase 2 constraint: every operation that mutates or projects a data
	// array runs in WASM, never in JS. This is the single chokepoint —
	// `Array.prototype.sort` is intentionally absent from this file.
	function applySort(rows: JobRun[], key: keyof JobRun, dir: SortDir): JobRun[] {
		// `$state.snapshot` strips the Svelte reactive Proxy; without it,
		// serde-wasm-bindgen sees an opaque proxy and refuses to deserialize.
		const plain = $state.snapshot(rows);
		return sortRunsBy(plain, key, dir) as JobRun[];
	}

	function handleSort(key: keyof JobRun) {
		if (sortKey === key) {
			sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			sortDir = 'asc';
		}
		runs = applySort(runs, sortKey, sortDir);
	}

	function sortIndicator(key: keyof JobRun): string {
		if (sortKey !== key) return '';
		return sortDir === 'asc' ? ' ▲' : ' ▼';
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
				{#each COLUMNS as col (col.key)}
					<th>
						<button type="button" class="sort-header" onclick={() => handleSort(col.key)}>
							{col.label}{sortIndicator(col.key)}
						</button>
					</th>
				{/each}
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
		padding: 0;
		border-bottom: 1px solid #2a2f38;
		font-weight: 500;
		color: #9aa3b2;
	}

	.sort-header {
		display: block;
		width: 100%;
		padding: 0.5rem 0.75rem;
		background: none;
		border: none;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
		user-select: none;
	}

	.sort-header:hover {
		background: #161a21;
		color: #e6e8eb;
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
