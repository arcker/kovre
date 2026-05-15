<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		getConfig,
		listJobs,
		listJobRuns,
		listSnapshots,
		putConfig,
		triggerRun,
		type Job,
		type JobRun,
		type Snapshot
	} from '$lib/api';
	import { emitConfigYaml, removeJob } from '$lib/yaml';
	import { formatBytes, formatTime, shortId } from '$lib/format';

	const jobName = $derived(page.params.name ?? '');

	let job = $state<Job | null>(null);
	let runs = $state<JobRun[]>([]);
	let snapshots = $state<Snapshot[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let triggering = $state(false);
	let triggerMessage = $state<string | null>(null);
	let pollHandle: ReturnType<typeof setInterval> | null = null;

	onMount(async () => {
		try {
			await refresh();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	onDestroy(() => {
		if (pollHandle) clearInterval(pollHandle);
	});

	async function refresh() {
		const [jobs, allRuns, allSnaps] = await Promise.all([
			listJobs(),
			listJobRuns(),
			listSnapshots()
		]);
		job = jobs.find((j) => j.name === jobName) ?? null;
		runs = allRuns.filter((r) => r.job_name === jobName);
		snapshots = allSnaps.filter((s) => s.job_name === jobName);
	}

	async function onRun() {
		triggering = true;
		triggerMessage = null;
		try {
			const id = await triggerRun(jobName);
			triggerMessage = `started run ${shortId(id)} — polling…`;
			startPolling();
		} catch (e) {
			triggerMessage = e instanceof Error ? e.message : String(e);
		} finally {
			triggering = false;
		}
	}

	function startPolling() {
		if (pollHandle) clearInterval(pollHandle);
		pollHandle = setInterval(async () => {
			await refresh();
			const stillRunning = runs.some((r) => r.status === 'running');
			if (!stillRunning && pollHandle) {
				clearInterval(pollHandle);
				pollHandle = null;
				triggerMessage = null;
			}
		}, 2000);
	}

	const isRunning = $derived(runs.some((r) => r.status === 'running'));

	async function onDelete() {
		if (!confirm(`Delete job "${jobName}"? The rustic snapshots are kept; only the kovre.yaml entry is removed.`)) return;
		try {
			const cfg = await getConfig();
			const yaml = emitConfigYaml(removeJob(cfg.parsed, jobName));
			await putConfig(yaml);
			goto('/');
		} catch (e) {
			triggerMessage = e instanceof Error ? e.message : String(e);
		}
	}
</script>

<div class="header">
	<h2>job: {jobName}</h2>
	<div class="header-actions">
		<a class="action" href={`/jobs/${jobName}/edit`}>edit</a>
		<button type="button" class="action delete" onclick={onDelete}>delete</button>
		<button type="button" class="run-btn" disabled={triggering || isRunning} onclick={onRun}>
			{isRunning ? 'running…' : triggering ? 'starting…' : 'Run now'}
		</button>
	</div>
</div>

{#if triggerMessage}
	<p class="info">{triggerMessage}</p>
{/if}

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else if !job}
	<p class="error">No job named <code>{jobName}</code> in kovre.yaml.</p>
{:else}
	<section class="meta">
		<dl>
			<dt>Repository</dt>
			<dd>{job.repository}</dd>
			<dt>Template</dt>
			<dd>{job.template ?? '(custom)'}</dd>
			{#if job.paths && job.paths.length > 0}
				<dt>Paths</dt>
				<dd class="mono">{job.paths.join(', ')}</dd>
			{/if}
			{#if job.excludes && job.excludes.length > 0}
				<dt>Excludes</dt>
				<dd class="mono">{job.excludes.join(', ')}</dd>
			{/if}
			{#if job.retention}
				<dt>Retention</dt>
				<dd>
					{#each Object.entries(job.retention) as [k, v]}
						{#if v != null}
							<span class="chip">{k.replace('keep_', '')}={v}</span>
						{/if}
					{/each}
				</dd>
			{/if}
		</dl>
	</section>

	<section>
		<h3>Recent runs</h3>
		{#if runs.length === 0}
			<p class="empty">No runs yet.</p>
		{:else}
			<table>
				<thead>
					<tr>
						<th>Started</th>
						<th>Finished</th>
						<th>Status</th>
						<th>Bytes</th>
						<th>Snapshot</th>
						<th>Run ID</th>
					</tr>
				</thead>
				<tbody>
					{#each runs as run (run.id)}
						<tr class={`row-${run.status}`}>
							<td>{formatTime(run.started_at)}</td>
							<td>{formatTime(run.finished_at)}</td>
							<td>{run.status}{#if run.failure_reason}<span class="reason"> ({run.failure_reason})</span>{/if}</td>
							<td>{formatBytes(run.bytes_processed)}</td>
							<td class="mono">{shortId(run.snapshot_id)}</td>
							<td class="mono">{shortId(run.id)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>

	<section>
		<h3>Snapshots</h3>
		{#if snapshots.length === 0}
			<p class="empty">No snapshots yet for this job.</p>
		{:else}
			<table>
				<thead>
					<tr>
						<th>Time</th>
						<th>Hostname</th>
						<th>Bytes</th>
						<th>Paths</th>
						<th>ID</th>
					</tr>
				</thead>
				<tbody>
					{#each snapshots as s (s.id)}
						<tr>
							<td>{formatTime(s.time)}</td>
							<td>{s.hostname}</td>
							<td>{formatBytes(s.bytes_total)}</td>
							<td class="mono">{s.paths.join(', ')}</td>
							<td class="mono">
								<a href="/snapshots/{jobName}/{s.id}">{shortId(s.id)}</a>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>
{/if}

<style>
	.header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin: 0 0 1.25rem;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}

	h3 {
		margin: 1.5rem 0 0.75rem;
		font-size: 0.95rem;
		font-weight: 500;
		color: #9aa3b2;
	}

	.header-actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.action {
		padding: 0.4rem 0.8rem;
		background: #1f242c;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		color: #9aa3b2;
		font: inherit;
		font-size: 0.9rem;
		text-decoration: none;
		cursor: pointer;
	}
	.action:hover {
		background: #262c36;
		color: #e6e8eb;
	}
	.action.delete {
		color: #d97070;
		border-color: #3a2a2a;
		background: #2a1f1f;
	}
	.action.delete:hover {
		background: #3a1f1f;
		color: #ff8a8a;
	}

	.run-btn {
		padding: 0.45rem 0.9rem;
		background: #2a4d8f;
		color: #e6e8eb;
		border: none;
		border-radius: 4px;
		cursor: pointer;
		font: inherit;
	}
	.run-btn:hover:not(:disabled) {
		background: #355fb0;
	}
	.run-btn:disabled {
		background: #2a2f38;
		color: #6a7180;
		cursor: not-allowed;
	}

	.info {
		color: #80a8e6;
		font-size: 0.9rem;
	}

	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.4rem 1rem;
		margin: 0;
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

	.chip {
		display: inline-block;
		padding: 0.1rem 0.5rem;
		margin-right: 0.4rem;
		background: #1f242c;
		border-radius: 3px;
		font-size: 0.85rem;
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
	.reason {
		color: #f47373;
		font-size: 0.85rem;
	}

	.mono {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
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
	.empty {
		color: #9aa3b2;
		font-size: 0.95rem;
	}
</style>
