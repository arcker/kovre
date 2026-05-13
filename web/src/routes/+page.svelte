<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getConfig,
		listJobs,
		listJobRuns,
		putConfig,
		triggerRun,
		type Job,
		type JobRun
	} from '$lib/api';
	import { emitConfigYaml, removeJob } from '$lib/yaml';
	import { formatBytes, formatRelative } from '$lib/format';

	interface TileState {
		job: Job;
		lastRun: JobRun | null;
		busy: boolean;
		message: string | null;
	}

	let tiles = $state<TileState[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			await refresh();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	async function refresh() {
		const [jobs, runs] = await Promise.all([listJobs(), listJobRuns()]);
		const byJob = lastRunByJob(runs);
		tiles = jobs.map((job) => {
			const previous = tiles.find((t) => t.job.name === job.name);
			return {
				job,
				lastRun: byJob.get(job.name) ?? null,
				busy: previous?.busy ?? false,
				message: previous?.message ?? null
			};
		});
	}

	function lastRunByJob(runs: JobRun[]): Map<string, JobRun> {
		const out = new Map<string, JobRun>();
		for (const r of runs) {
			const prev = out.get(r.job_name);
			if (!prev || r.started_at > prev.started_at) out.set(r.job_name, r);
		}
		return out;
	}

	async function runJob(name: string) {
		const tile = tiles.find((t) => t.job.name === name);
		if (!tile || tile.busy) return;
		tile.busy = true;
		tile.message = 'starting…';
		try {
			await triggerRun(name);
			tile.message = null;
			await pollUntilDone(name);
		} catch (e) {
			tile.message = e instanceof Error ? e.message : String(e);
		} finally {
			tile.busy = false;
		}
	}

	async function pollUntilDone(name: string): Promise<void> {
		while (true) {
			await new Promise((r) => setTimeout(r, 2000));
			await refresh();
			const tile = tiles.find((t) => t.job.name === name);
			if (!tile || tile.lastRun?.status !== 'running') return;
		}
	}

	async function deleteJob(name: string) {
		if (!confirm(`Delete job "${name}"? The rustic snapshots are kept; only the kovre.yaml entry is removed.`)) return;
		try {
			const cfg = await getConfig();
			const yaml = emitConfigYaml(removeJob(cfg.parsed, name));
			await putConfig(yaml);
			await refresh();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	function templateIcon(template: string | null | undefined): string {
		switch (template) {
			case 'documents':
				return '📄';
			case 'dev-repos':
				return '⚙️';
			case 'steam-saves':
				return '🎮';
			default:
				return '📂';
		}
	}

	function templateLabel(job: Job): string {
		return job.template ?? 'custom';
	}

	function statusKind(run: JobRun | null): string {
		return run?.status ?? 'never';
	}

	function statusText(run: JobRun | null): string {
		if (!run) return 'never run';
		if (run.status === 'running') return 'running';
		if (run.status === 'success') return 'success';
		if (run.status === 'failed') return 'failed';
		return run.status;
	}

	function retentionSummary(job: Job): string {
		const r = job.retention;
		if (!r) return '';
		const parts: string[] = [];
		for (const [k, v] of Object.entries(r)) {
			if (v == null) continue;
			parts.push(`${k.replace('keep_', '')}=${v}`);
		}
		return parts.join(' · ');
	}

	// What gets backed up. For custom jobs, the explicit `paths`. For
	// template jobs, the template_options (scan_root, etc.) when set,
	// otherwise a placeholder — the real paths are resolved at run
	// time by the template engine and not exposed here in Phase 2.
	function sourceLines(job: Job): string[] {
		if (job.paths && job.paths.length > 0) return job.paths;
		const opts = job.template_options as Record<string, unknown> | null | undefined;
		if (opts && typeof opts === 'object') {
			const entries = Object.entries(opts).filter(([, v]) => v != null);
			if (entries.length > 0) return entries.map(([k, v]) => `${k}: ${v}`);
		}
		if (job.template) return [`(resolved at run time by template "${job.template}")`];
		return ['—'];
	}
</script>

<h2>Overview</h2>

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else if tiles.length === 0}
	<p class="empty">No jobs declared in <code>kovre.yaml</code>.</p>
{:else}
	<div class="grid">
		{#each tiles as tile (tile.job.name)}
			{@const run = tile.lastRun}
			{@const kind = statusKind(run)}
			<article class={`tile kind-${kind}`}>
				<header>
					<span class="icon">{templateIcon(tile.job.template)}</span>
					<a class="name" href="/jobs/{tile.job.name}">{tile.job.name}</a>
					<span class="template">{templateLabel(tile.job)}</span>
				</header>

				<div class="status-row">
					<span class={`pill kind-${kind}`}>
						{statusText(run)}
					</span>
					<span class="when">{run ? formatRelative(run.started_at) : ''}</span>
				</div>

				<dl class="meta">
					<dt>Source</dt>
					<dd class="source">
						{#each sourceLines(tile.job) as line}
							<span class="path">{line}</span>
						{/each}
					</dd>
					{#if run?.bytes_processed != null}
						<dt>Last size</dt>
						<dd>{formatBytes(run.bytes_processed)}</dd>
					{/if}
					{#if run?.failure_reason}
						<dt>Reason</dt>
						<dd class="reason">{run.failure_reason}</dd>
					{/if}
					{#if retentionSummary(tile.job)}
						<dt>Retention</dt>
						<dd>{retentionSummary(tile.job)}</dd>
					{/if}
					<dt>Repo</dt>
					<dd>{tile.job.repository}</dd>
				</dl>

				{#if tile.message}
					<p class="msg">{tile.message}</p>
				{/if}

				<footer>
					<button
						type="button"
						class="run"
						disabled={tile.busy || run?.status === 'running'}
						onclick={() => runJob(tile.job.name)}
					>
						{tile.busy || run?.status === 'running' ? '⟳ running' : '▶ Run now'}
					</button>
					<a class="action" href="/jobs/{tile.job.name}/edit">edit</a>
					<button
						type="button"
						class="action delete"
						onclick={() => deleteJob(tile.job.name)}
						disabled={tile.busy || run?.status === 'running'}
					>
						delete
					</button>
					<a class="open" href="/jobs/{tile.job.name}">details →</a>
				</footer>
			</article>
		{/each}
	</div>
{/if}

<style>
	h2 {
		margin: 0 0 1.25rem;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(440px, 1fr));
		gap: 1.5rem;
	}

	.tile {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		padding: 1.75rem 2rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 10px;
		min-height: 320px;
	}

	.tile.kind-failed {
		border-color: #5a2a2a;
	}
	.tile.kind-running {
		border-color: #5a4a1f;
	}
	.tile.kind-success {
		border-color: #2a4d3f;
	}

	header {
		display: flex;
		align-items: center;
		gap: 1rem;
	}
	.icon {
		font-size: 2.6rem;
		line-height: 1;
	}
	.name {
		color: #e6e8eb;
		text-decoration: none;
		font-weight: 600;
		font-size: 1.6rem;
		flex: 1;
	}
	.name:hover {
		color: #80a8e6;
	}
	.template {
		font-size: 0.85rem;
		color: #9aa3b2;
		background: #1f242c;
		padding: 0.3rem 0.75rem;
		border-radius: 14px;
		text-transform: lowercase;
	}

	.status-row {
		display: flex;
		align-items: baseline;
		gap: 0.85rem;
	}

	.pill {
		display: inline-block;
		padding: 0.35rem 0.95rem;
		border-radius: 5px;
		font-size: 1rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
	.pill.kind-success {
		background: #1f3a2c;
		color: #6ad08e;
	}
	.pill.kind-failed {
		background: #3a1f1f;
		color: #f47373;
	}
	.pill.kind-running {
		background: #3a341f;
		color: #f5d36a;
	}
	.pill.kind-never {
		background: #1f242c;
		color: #9aa3b2;
	}

	.when {
		color: #9aa3b2;
		font-size: 0.95rem;
	}

	.meta {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.5rem 1rem;
		margin: 0;
		font-size: 0.95rem;
	}
	.meta dt {
		color: #6a7180;
	}
	.meta dd {
		margin: 0;
		color: #c5cad3;
	}
	.meta .reason {
		color: #f47373;
	}

	.meta .source {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}
	.meta .path {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
		color: #c5cad3;
		overflow-wrap: anywhere;
	}

	.msg {
		margin: 0;
		padding: 0.55rem 0.8rem;
		background: #1f242c;
		border-radius: 4px;
		font-size: 0.95rem;
		color: #80a8e6;
	}

	footer {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-top: auto;
		padding-top: 0.85rem;
		border-top: 1px solid #2a2f38;
	}

	.run {
		padding: 0.65rem 1.4rem;
		background: #2a4d8f;
		color: #e6e8eb;
		border: none;
		border-radius: 5px;
		cursor: pointer;
		font: inherit;
		font-size: 1rem;
		font-weight: 500;
	}
	.run:hover:not(:disabled) {
		background: #355fb0;
	}
	.run:disabled {
		background: #2a2f38;
		color: #6a7180;
		cursor: not-allowed;
	}

	.action {
		padding: 0.4rem 0.7rem;
		background: #1f242c;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		color: #9aa3b2;
		font: inherit;
		font-size: 0.85rem;
		text-decoration: none;
		cursor: pointer;
	}
	.action:hover:not(:disabled) {
		background: #262c36;
		color: #e6e8eb;
	}
	.action.delete {
		color: #d97070;
	}
	.action.delete:hover:not(:disabled) {
		background: #2a1f1f;
		color: #ff8a8a;
	}
	.action:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.open {
		margin-left: auto;
		color: #9aa3b2;
		text-decoration: none;
		font-size: 0.95rem;
	}
	.open:hover {
		color: #e6e8eb;
	}

	.error {
		color: #f47373;
	}
	.empty {
		color: #9aa3b2;
		font-size: 0.95rem;
	}
</style>
