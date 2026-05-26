<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		browseJob,
		getConfig,
		getRestoreRun,
		listFileVersions,
		listJobs,
		previewUrl,
		triggerRestore,
		triggerSelectiveRestore,
		type BrowseEntry,
		type BrowseResult,
		type ConfigPayload,
		type Job,
		type RestoreRun,
		type VersionEntry
	} from '$lib/api';
	import { formatBytes, formatRelative } from '$lib/format';
	import DirInput from '$lib/DirInput.svelte';

	const jobName = $derived(page.params.name ?? '');

	let job = $state<Job | null>(null);
	let config = $state<ConfigPayload | null>(null);
	let destDir = $state('');
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	// ---- Browse state ----
	let browseResult = $state<BrowseResult | null>(null);
	let browsePath = $state('');
	let browseLoading = $state(false);
	let browseUnsupported = $state(false);

	// ---- File detail panel ----
	let selectedFile = $state<BrowseEntry | null>(null);
	let selectedFilePath = $state('');
	let fileVersions = $state<VersionEntry[]>([]);
	let versionsLoading = $state(false);

	// ---- Restore state ----
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
			const today = new Date().toISOString().slice(0, 10);
			destDir = `C:\\kovre-restore\\${jobName}\\${today}`;
			// Start initial browse (non-blocking for the page load).
			loadBrowse('');
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	async function loadBrowse(subpath: string) {
		browseLoading = true;
		browsePath = subpath;
		const result = await browseJob(jobName, subpath);
		browseLoading = false;
		if (result) {
			browseResult = result;
			browseUnsupported = false;
		} else {
			browseResult = null;
			browseUnsupported = true;
		}
	}

	function navigateBrowse(name: string) {
		const next = browsePath ? `${browsePath}/${name}` : name;
		loadBrowse(next);
	}

	function browseUp() {
		const parts = browsePath.split('/').filter((s) => s.length > 0);
		parts.pop();
		loadBrowse(parts.join('/'));
	}

	const breadcrumb = $derived(
		browsePath
			.split('/')
			.filter((s) => s.length > 0)
	);

	async function selectFile(entry: BrowseEntry) {
		selectedFile = entry;
		selectedFilePath = browsePath ? `${browsePath}/${entry.name}` : entry.name;
		fileVersions = [];
		versionsLoading = true;
		fileVersions = await listFileVersions(jobName, selectedFilePath);
		versionsLoading = false;
	}

	function closeDetail() {
		selectedFile = null;
		selectedFilePath = '';
		fileVersions = [];
	}

	const previewSrc = $derived(
		selectedFile && !selectedFile.is_dir
			? previewUrl(jobName, selectedFilePath)
			: ''
	);
	const isImage = $derived(
		selectedFile?.name.match(/\.(jpe?g|png|gif|webp|bmp|svg|ico)$/i) != null
	);
	const isText = $derived(
		selectedFile?.name.match(/\.(txt|md|json|ya?ml|toml|ini|cfg|log|csv|xml|html?)$/i) != null
	);

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
		<!-- Browse: what's in this backup -->
		{#if browseUnsupported}
			<section class="browse-info">
				<p>This backup uses the <strong>rustic</strong> backend (encrypted). Browse is not
				available — use <code>rustic ls</code> CLI to inspect snapshot contents.</p>
			</section>
		{:else if browseResult}
			<section class="browse">
				<h3>What's in this backup</h3>
				<nav class="breadcrumb">
					<button
						type="button"
						class="crumb"
						class:current={browsePath === ''}
						onclick={() => loadBrowse('')}
					>
						/
					</button>
					{#each breadcrumb as segment, i}
						<span class="sep">/</span>
						<button
							type="button"
							class="crumb"
							class:current={i === breadcrumb.length - 1}
							onclick={() => loadBrowse(breadcrumb.slice(0, i + 1).join('/'))}
						>
							{segment}
						</button>
					{/each}
				</nav>
				{#if browseLoading}
					<p class="muted">Loading…</p>
				{:else}
					{#if browsePath !== ''}
						<button type="button" class="entry dir" onclick={browseUp}>
							<span class="entry-icon">⬆</span>
							<span class="entry-name">..</span>
						</button>
					{/if}
					{#each browseResult.entries as entry (entry.name)}
						{#if entry.is_dir}
							<button
								type="button"
								class="entry dir"
								onclick={() => navigateBrowse(entry.name)}
							>
								<span class="entry-icon">📁</span>
								<span class="entry-name">{entry.name}</span>
							</button>
						{:else}
							<button
								type="button"
								class="entry file"
								class:selected={selectedFile?.name === entry.name && !selectedFile?.is_dir}
								onclick={() => selectFile(entry)}
							>
								<span class="entry-icon">📄</span>
								<span class="entry-name">{entry.name}</span>
								{#if entry.versions_count > 0}
									<span class="entry-versions" title="{entry.versions_count} archived version(s)">
										{entry.versions_count}v
									</span>
								{/if}
								{#if entry.modified}
									<span class="entry-date">{formatRelative(entry.modified)}</span>
								{/if}
								{#if entry.size != null}
									<span class="entry-size">{formatBytes(entry.size)}</span>
								{/if}
							</button>
						{/if}
					{/each}
					{#if browseResult.entries.length === 0}
						<p class="muted">Empty directory.</p>
					{/if}
				{/if}
			</section>
		{/if}

		<!-- File detail panel -->
		{#if selectedFile && !selectedFile.is_dir}
			<section class="detail">
				<div class="detail-head">
					<h3>{selectedFile.name}</h3>
					<button type="button" class="detail-close" onclick={closeDetail}>×</button>
				</div>
				<dl class="detail-meta">
					<dt>Path</dt>
					<dd class="mono">{selectedFilePath}</dd>
					<dt>Size</dt>
					<dd>{selectedFile.size != null ? formatBytes(selectedFile.size) : '—'}</dd>
					{#if selectedFile.modified}
						<dt>Modified</dt>
						<dd>{formatRelative(selectedFile.modified)}</dd>
					{/if}
				</dl>

				{#if isImage}
					<img class="preview-img" src={previewSrc} alt={selectedFile.name} />
				{:else if isText}
					<iframe class="preview-text" src={previewSrc} title="Preview"></iframe>
				{/if}

				{#if versionsLoading}
					<p class="muted">Loading versions…</p>
				{:else if fileVersions.length > 0}
					<h4>Archived versions ({fileVersions.length})</h4>
					<ul class="versions-list">
						{#each fileVersions as v (v.name)}
							<li class="version-item">
								<span class="version-ts">{v.timestamp}</span>
								<span class="version-size">{formatBytes(v.size)}</span>
							</li>
						{/each}
					</ul>
				{:else}
					<p class="muted">No archived versions.</p>
				{/if}

				<div class="detail-actions">
					<button
						type="button"
						class="submit"
						onclick={() => {
							destDir = `C:\\kovre-restore\\${jobName}\\${new Date().toISOString().slice(0, 10)}`;
							submit();
						}}
					>
						↻ Restore this file
					</button>
				</div>
			</section>
		{/if}

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

	.browse-info {
		padding: 0.8rem 1rem;
		background: #1f242c;
		border-left: 3px solid #6a7180;
		border-radius: 4px;
		max-width: 640px;
		margin-bottom: 1.2rem;
	}
	.browse-info p {
		margin: 0;
		color: #9aa3b2;
		font-size: 0.88rem;
	}
	.browse-info code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #c5cad3;
	}

	.browse {
		max-width: 640px;
		margin-bottom: 1.5rem;
		padding: 1rem 1.2rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 6px;
	}
	.browse h3 {
		margin: 0 0 0.6rem;
		font-size: 0.95rem;
		font-weight: 500;
		color: #c5cad3;
	}

	.breadcrumb {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.1rem;
		margin-bottom: 0.7rem;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.82rem;
	}
	.crumb {
		padding: 0.15rem 0.4rem;
		background: transparent;
		border: none;
		color: #80a8e6;
		cursor: pointer;
		font: inherit;
		border-radius: 3px;
	}
	.crumb:hover {
		background: #1d2a3f;
	}
	.crumb.current {
		color: #e6e8eb;
		font-weight: 500;
		cursor: default;
	}
	.sep {
		color: #4a5564;
	}

	.entry {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		padding: 0.35rem 0.5rem;
		border: none;
		border-radius: 3px;
		background: transparent;
		text-align: left;
		font: inherit;
		font-size: 0.88rem;
		color: #c5cad3;
	}
	.entry.dir,
	.entry.file {
		cursor: pointer;
	}
	.entry.dir:hover,
	.entry.file:hover {
		background: #1d2a3f;
		color: #e6e8eb;
	}
	.entry.file.selected {
		background: #1d2a3f;
		border-left: 2px solid #80a8e6;
	}
	.entry-versions {
		color: #f5d36a;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.72rem;
		background: #3a341f;
		padding: 0.05rem 0.35rem;
		border-radius: 3px;
		flex-shrink: 0;
	}
	.entry-date {
		color: #6a7180;
		font-size: 0.75rem;
		flex-shrink: 0;
	}
	.entry-icon {
		width: 1.2rem;
		text-align: center;
		flex-shrink: 0;
	}
	.entry-name {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
	}
	.entry-size {
		color: #6a7180;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.78rem;
		flex-shrink: 0;
	}

	.detail {
		max-width: 640px;
		margin-bottom: 1.5rem;
		padding: 1rem 1.2rem;
		background: #161a21;
		border: 1px solid #2a4d8f;
		border-radius: 6px;
	}
	.detail-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.6rem;
	}
	.detail-head h3 {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 500;
		color: #e6e8eb;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
	}
	.detail-close {
		background: transparent;
		border: 1px solid #2a2f38;
		border-radius: 3px;
		color: #6a7180;
		font-size: 1rem;
		cursor: pointer;
		width: 1.6rem;
		height: 1.6rem;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}
	.detail-close:hover {
		color: #e6e8eb;
		background: #1f242c;
	}
	.detail-meta {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.25rem 0.7rem;
		margin: 0 0 0.8rem;
		font-size: 0.85rem;
	}
	.detail-meta dt {
		color: #6a7180;
	}
	.detail-meta dd {
		margin: 0;
		color: #c5cad3;
	}

	.preview-img {
		max-width: 100%;
		max-height: 300px;
		border-radius: 4px;
		margin-bottom: 0.8rem;
		background: #0a0c10;
	}
	.preview-text {
		width: 100%;
		height: 200px;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		background: #0f1115;
		margin-bottom: 0.8rem;
	}

	.detail h4 {
		margin: 0.5rem 0 0.3rem;
		font-size: 0.85rem;
		font-weight: 500;
		color: #c5cad3;
	}
	.versions-list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}
	.version-item {
		display: flex;
		gap: 0.6rem;
		padding: 0.25rem 0.4rem;
		border-radius: 3px;
		font-size: 0.82rem;
	}
	.version-item:hover {
		background: #1a1f27;
	}
	.version-ts {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #9aa3b2;
	}
	.version-size {
		color: #6a7180;
		margin-left: auto;
	}
	.detail-actions {
		margin-top: 0.8rem;
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
