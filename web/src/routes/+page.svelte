<script lang="ts">
	import { onMount } from 'svelte';
	import { listJobs, listJobRuns, type Job, type JobRun } from '$lib/api';
	import { formatTime } from '$lib/format';

	interface Row {
		job: Job;
		lastRun: JobRun | null;
	}

	let rows = $state<Row[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [jobs, runs] = await Promise.all([listJobs(), listJobRuns()]);
			rows = buildRows(jobs, runs);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	// Pure projection (no sort/filter via JS) — picks the most recent
	// run per job by `started_at`. The "most recent" comparison is a
	// scalar pick, not a sort, so it's allowed in JS without breaking
	// the WASM-only sort rule.
	function buildRows(jobs: Job[], runs: JobRun[]): Row[] {
		const lastByJob = new Map<string, JobRun>();
		for (const r of runs) {
			const prev = lastByJob.get(r.job_name);
			if (!prev || r.started_at > prev.started_at) {
				lastByJob.set(r.job_name, r);
			}
		}
		return jobs.map((job) => ({ job, lastRun: lastByJob.get(job.name) ?? null }));
	}
</script>

<h2>Overview</h2>

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else if rows.length === 0}
	<p class="empty">No jobs declared in <code>kovre.yaml</code>.</p>
{:else}
	<table>
		<thead>
			<tr>
				<th>Job</th>
				<th>Repository</th>
				<th>Template</th>
				<th>Last run</th>
				<th>Status</th>
				<th></th>
			</tr>
		</thead>
		<tbody>
			{#each rows as row (row.job.name)}
				<tr>
					<td><a href="/jobs/{row.job.name}">{row.job.name}</a></td>
					<td>{row.job.repository}</td>
					<td>{row.job.template ?? '(custom)'}</td>
					<td>{formatTime(row.lastRun?.started_at ?? null)}</td>
					<td class={row.lastRun ? `status-${row.lastRun.status}` : 'status-none'}>
						{row.lastRun?.status ?? '—'}
					</td>
					<td><a class="cta" href="/jobs/{row.job.name}">open →</a></td>
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

	a {
		color: #80a8e6;
		text-decoration: none;
	}
	a:hover {
		text-decoration: underline;
	}
	.cta {
		color: #9aa3b2;
		font-size: 0.85rem;
	}

	.status-running {
		color: #f5d36a;
	}
	.status-failed {
		color: #f47373;
	}
	.status-success {
		color: #6ad08e;
	}
	.status-none {
		color: #5e6571;
	}

	.error {
		color: #f47373;
	}
	.empty {
		color: #9aa3b2;
		font-size: 0.95rem;
	}
</style>
