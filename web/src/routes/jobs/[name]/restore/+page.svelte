<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		getConfig,
		getRestoreRun,
		listJobs,
		triggerRestore,
		type ConfigPayload,
		type Job,
		type RestoreRun
	} from '$lib/api';
	import { formatRelative } from '$lib/format';
	import DirInput from '$lib/DirInput.svelte';

	const jobName = $derived(page.params.name ?? '');

	let job = $state<Job | null>(null);
	let config = $state<ConfigPayload | null>(null);
	let destDir = $state('');
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	let submitting = $state(false);
	let runId = $state<string | null>(null);
	let run = $state<RestoreRun | null>(null);
	let pollHandle = $state<ReturnType<typeof setTimeout> | null>(null);
	let pollError = $state<string | null>(null);
	let submitError = $state<string | null>(null);

	const isTerminal = $derived(
		run !== null && (run.status === 'success' || run.status === 'failed')
	);
	const isRunning = $derived(run !== null && run.status === 'running');
	const repoBackend = $derived(
		job && config
			? (config.parsed.repositories?.[job.repository]?.backend ?? 'rustic')
			: 'rustic'
	);

	onMount(async () => {
		try {
			const [jobs, cfg] = await Promise.all([listJobs(), getConfig()]);
			config = cfg;
			job = jobs.find((j) => j.name === jobName) ?? null;
			if (!job) {
				loadError = `No job named "${jobName}" in kovre.yaml`;
				return;
			}
			// Default destination: C:\kovre-restore\<job>\<YYYY-MM-DD>
			// (the user can edit; this is just a sane suggestion to
			// avoid accidental writes to e.g. the source folder).
			const today = new Date().toISOString().slice(0, 10);
			destDir = `C:\\kovre-restore\\${jobName}\\${today}`;
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	onDestroy(() => {
		if (pollHandle != null) clearTimeout(pollHandle);
	});

	async function submit() {
		if (!job || submitting) return;
		submitting = true;
		submitError = null;
		pollError = null;
		run = null;
		runId = null;
		try {
			const id = await triggerRestore(jobName, destDir);
			runId = id;
			// Server returned 202 → run was created with status=running.
			// Seed `run` locally so the UI flips into the running view
			// without waiting for the first poll round-trip.
			run = {
				id,
				job_name: jobName,
				dest_dir: destDir,
				started_at: new Date().toISOString(),
				finished_at: null,
				status: 'running',
				failure_reason: null,
				trigger: 'dashboard'
			};
			scheduleNextPoll();
		} catch (e) {
			submitError = e instanceof Error ? e.message : String(e);
		} finally {
			submitting = false;
		}
	}

	function scheduleNextPoll() {
		if (runId == null) return;
		pollHandle = setTimeout(pollOnce, 2000);
	}

	async function pollOnce() {
		if (runId == null) return;
		try {
			const fresh = await getRestoreRun(runId);
			if (fresh) run = fresh;
			pollError = null;
		} catch (e) {
			pollError = e instanceof Error ? e.message : String(e);
		}
		// Keep polling until terminal.
		if (run && (run.status === 'success' || run.status === 'failed')) {
			pollHandle = null;
			return;
		}
		scheduleNextPoll();
	}
</script>

<a class="back" href={`/jobs/${encodeURIComponent(jobName)}`}>← back to {jobName}</a>

{#if loading}
	<p>Loading…</p>
{:else if loadError}
	<p class="error">{loadError}</p>
{:else if job}
	<h2>Restore <code>{jobName}</code></h2>
	<p class="lead">
		Reads from <strong>{job.repository}</strong>
		(<span class={`badge backend-${repoBackend}`}>{repoBackend}</span>)
		and writes the latest state into the destination folder below.
		The original source paths are <strong>never touched</strong>. Existing
		files at the destination will be overwritten.
	</p>

	{#if !run}
		<form
			onsubmit={(e) => {
				e.preventDefault();
				submit();
			}}
		>
			<label>
				<span class="label">Destination folder</span>
				<DirInput bind:value={destDir} placeholder="C:\restore\..." />
				<span class="hint">
					Folder where the restored files will be written. Created if it doesn't exist.
				</span>
			</label>

			{#if submitError}
				<p class="error">{submitError}</p>
			{/if}

			<div class="actions">
				<button type="submit" class="submit" disabled={submitting || destDir.trim() === ''}>
					{submitting ? 'starting…' : 'Restore'}
				</button>
				<a href={`/jobs/${encodeURIComponent(jobName)}`} class="cancel">cancel</a>
			</div>
		</form>
	{:else}
		<section class="status">
			<div class="status-head">
				<span class={`pill kind-${run.status}`}>{run.status}</span>
				<span class="when">
					started {formatRelative(run.started_at)}
					{#if run.finished_at}
						· finished {formatRelative(run.finished_at)}
					{/if}
				</span>
			</div>

			<dl class="meta">
				<dt>Destination</dt>
				<dd class="mono">{run.dest_dir}</dd>
				<dt>Run id</dt>
				<dd class="mono">{run.id}</dd>
			</dl>

			{#if isRunning}
				<div class="progress" role="progressbar" aria-label="restore in progress">
					<div class="bar"></div>
				</div>
				<p class="muted">
					Copying files… you can leave this page; the restore keeps running in the
					background. The run id above lets you check the result later.
				</p>
				{#if pollError}
					<p class="warn">Polling hiccup: {pollError} — will retry in 2s.</p>
				{/if}
			{:else if run.status === 'success'}
				<p class="success">
					✓ Restore complete. Files have been written to <code>{run.dest_dir}</code>.
				</p>
				<div class="actions">
					<a href="/" class="submit">back to inventory</a>
				</div>
			{:else if run.status === 'failed'}
				<p class="error">
					✗ Restore failed{run.failure_reason ? `: ${run.failure_reason}` : '.'}
				</p>
				<div class="actions">
					<a href={`/jobs/${encodeURIComponent(jobName)}`} class="cancel">back to job</a>
				</div>
			{/if}
		</section>
	{/if}
{/if}

<style>
	.back {
		display: inline-block;
		margin-bottom: 1rem;
		color: #9aa3b2;
		text-decoration: none;
		font-size: 0.9rem;
	}
	.back:hover {
		color: #e6e8eb;
	}

	h2 {
		margin: 0 0 0.6rem;
		font-size: 1.25rem;
		font-weight: 500;
		color: #e6e8eb;
	}
	h2 code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #80a8e6;
		font-weight: 500;
	}

	.lead {
		color: #9aa3b2;
		max-width: 720px;
		margin: 0 0 1.5rem;
		font-size: 0.92rem;
	}
	.lead strong {
		color: #e6e8eb;
		font-weight: 500;
	}

	.badge {
		display: inline-block;
		padding: 0.05rem 0.4rem;
		border-radius: 3px;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.72rem;
		text-transform: uppercase;
	}
	.backend-rustic {
		background: #1d2a3f;
		color: #80a8e6;
		border: 1px solid #2a4d8f;
	}
	.backend-mirror {
		background: #1f3a2c;
		color: #6ad08e;
		border: 1px solid #2a4d3f;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 640px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.label {
		color: #c5cad3;
		font-size: 0.9rem;
		font-weight: 500;
	}
	.hint {
		color: #6a7180;
		font-size: 0.82rem;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 0.85rem;
		margin-top: 0.5rem;
	}
	.submit {
		padding: 0.55rem 1.2rem;
		background: #2a4d8f;
		color: #e6e8eb;
		border: none;
		border-radius: 4px;
		text-decoration: none;
		cursor: pointer;
		font: inherit;
		font-weight: 500;
		font-size: 0.95rem;
	}
	.submit:hover:not(:disabled) {
		background: #355fb0;
	}
	.submit:disabled {
		background: #2a2f38;
		color: #6a7180;
		cursor: not-allowed;
	}
	.cancel {
		color: #9aa3b2;
		text-decoration: none;
		font-size: 0.9rem;
	}
	.cancel:hover {
		color: #e6e8eb;
	}

	.status {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		max-width: 720px;
		padding: 1.1rem 1.3rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 6px;
	}
	.status-head {
		display: flex;
		align-items: baseline;
		gap: 0.8rem;
	}
	.pill {
		display: inline-block;
		padding: 0.25rem 0.65rem;
		border-radius: 4px;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.78rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
	.pill.kind-running {
		background: #1d2a3f;
		color: #80a8e6;
	}
	.pill.kind-success {
		background: #1f3a2c;
		color: #6ad08e;
	}
	.pill.kind-failed {
		background: #3a1f1f;
		color: #f47373;
	}
	.when {
		color: #6a7180;
		font-size: 0.85rem;
	}

	.meta {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.3rem 0.85rem;
		margin: 0;
		font-size: 0.88rem;
	}
	.meta dt {
		color: #6a7180;
	}
	.meta dd {
		margin: 0;
		color: #c5cad3;
	}
	.mono {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		overflow-wrap: anywhere;
	}

	.progress {
		position: relative;
		height: 6px;
		background: #1a1f27;
		border-radius: 3px;
		overflow: hidden;
	}
	.bar {
		position: absolute;
		left: 0;
		top: 0;
		bottom: 0;
		width: 30%;
		background: linear-gradient(90deg, #2a4d8f, #80a8e6, #2a4d8f);
		animation: slide 1.4s linear infinite;
	}
	@keyframes slide {
		0% {
			transform: translateX(-100%);
		}
		100% {
			transform: translateX(400%);
		}
	}

	.muted {
		color: #9aa3b2;
		font-size: 0.85rem;
		margin: 0;
	}
	.warn {
		color: #f5d36a;
		font-size: 0.85rem;
		margin: 0;
	}
	.success {
		color: #6ad08e;
		margin: 0;
		font-size: 0.95rem;
	}
	.success code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #c5cad3;
	}
	.error {
		color: #f47373;
		margin: 0;
		font-size: 0.95rem;
	}
</style>
