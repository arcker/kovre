<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getConfig,
		getRepositoriesStatus,
		listJobs,
		listJobRuns,
		listTemplates,
		putConfig,
		resolveTemplate,
		triggerRun,
		type Job,
		type JobRun,
		type RepositoryStatus,
		type Template
	} from '$lib/api';
	import { emitConfigYaml, removeJob } from '$lib/yaml';
	import { formatBytes, formatRelative } from '$lib/format';

	// ---- Category mapping ------------------------------------------------
	//
	// Templates → categories. Hardcoded here on purpose: the inventory's
	// taxonomy is a UX concern, not a backend concern, and it changes
	// slower than the rest of the app. A job without `template` (= custom)
	// lands in the "Other" bucket.
	const CATEGORY_OF_TEMPLATE: Record<string, string> = {
		'user-files': 'personal',
		documents: 'personal', // YAML compat alias
		'thunderbird-mail': 'mails',
		'browser-profiles': 'browsers',
		'steam-saves': 'games',
		'dev-repos': 'dev',
		'user-appdata': 'appdata'
	};

	interface CategoryDef {
		id: string;
		icon: string;
		label: string;
		description: string;
		/** Which templates feed this category (drives the "you don't back
		 *  this up yet" suggestions). Empty for `other` (custom jobs only). */
		templates: string[];
	}

	const CATEGORIES: CategoryDef[] = [
		{
			id: 'personal',
			icon: '📄',
			label: 'Personal files',
			description: 'Documents, photos, music, videos, downloads, game saves.',
			templates: ['user-files']
		},
		{
			id: 'mails',
			icon: '📨',
			label: 'Mails',
			description: 'Mail clients (Thunderbird and friends).',
			templates: ['thunderbird-mail']
		},
		{
			id: 'browsers',
			icon: '🌐',
			label: 'Browsers',
			description: 'Bookmarks, history, logins, extensions, preferences.',
			templates: ['browser-profiles']
		},
		{
			id: 'games',
			icon: '🎮',
			label: 'Game saves',
			description: 'Auto-detected via the Ludusavi manifest.',
			templates: ['steam-saves']
		},
		{
			id: 'dev',
			icon: '⚙️',
			label: 'Dev repositories',
			description: 'Every git repo under your scan root.',
			templates: ['dev-repos']
		},
		{
			id: 'appdata',
			icon: '🗂️',
			label: 'App data',
			description: 'Safety net over %APPDATA% (Roaming), cache and temp excluded.',
			templates: ['user-appdata']
		},
		{
			id: 'other',
			icon: '📂',
			label: 'Other',
			description: 'Manual jobs (custom paths, no template).',
			templates: []
		}
	];

	function categoryOf(job: Job): string {
		if (!job.template) return 'other';
		return CATEGORY_OF_TEMPLATE[job.template] ?? 'other';
	}

	// ---- State -----------------------------------------------------------

	interface JobView {
		job: Job;
		lastRun: JobRun | null;
		resolved: { paths: string[]; status: string } | null;
		resolveError: string | null;
		repoBackend: 'rustic' | 'mirror' | null;
		busy: boolean;
		actionMessage: string | null;
	}

	let views = $state<JobView[]>([]);
	let templates = $state<Template[]>([]);
	let repoStatus = $state<Record<string, RepositoryStatus>>({});
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [jobs, runs, tmpls, cfg, rStatus] = await Promise.all([
				listJobs(),
				listJobRuns(),
				listTemplates(),
				getConfig(),
				getRepositoriesStatus()
			]);
			templates = tmpls;
			repoStatus = rStatus;
			const lastRunByJob = mapLastRun(runs);
			const repoBackendByName = new Map<string, 'rustic' | 'mirror'>();
			for (const [name, repo] of Object.entries(cfg.parsed.repositories ?? {})) {
				repoBackendByName.set(name, (repo.backend ?? 'rustic') as 'rustic' | 'mirror');
			}

			// Resolve every template-backed job in parallel — each takes
			// at most a few ms (documents) or up to a few seconds
			// (dev-repos scanning a large tree, steam-saves hitting the
			// registry). Total wall-clock is bounded by the slowest one.
			views = await Promise.all(
				jobs.map(async (job) => {
					let resolved: JobView['resolved'] = null;
					let resolveError: string | null = null;
					if (job.template) {
						try {
							const r = await resolveTemplate(
								job.template,
								job.template_options as Record<string, unknown> | null
							);
							if (r) resolved = { paths: r.paths, status: r.status };
						} catch (e) {
							resolveError = e instanceof Error ? e.message : String(e);
						}
					} else if (job.paths && job.paths.length > 0) {
						// Custom job — paths are explicit, no resolution needed.
						resolved = { paths: job.paths, status: 'ok' };
					}
					return {
						job,
						lastRun: lastRunByJob.get(job.name) ?? null,
						resolved,
						resolveError,
						repoBackend: repoBackendByName.get(job.repository) ?? null,
						busy: false,
						actionMessage: null
					};
				})
			);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

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

	function mapLastRun(runs: JobRun[]): Map<string, JobRun> {
		const out = new Map<string, JobRun>();
		for (const r of runs) {
			const prev = out.get(r.job_name);
			if (!prev || r.started_at > prev.started_at) out.set(r.job_name, r);
		}
		return out;
	}

	// ---- Actions --------------------------------------------------------

	async function runJob(name: string) {
		const view = views.find((v) => v.job.name === name);
		if (!view || view.busy) return;
		view.busy = true;
		view.actionMessage = 'starting…';
		try {
			await triggerRun(name);
			view.actionMessage = null;
			await pollJobUntilDone(name);
		} catch (e) {
			view.actionMessage = e instanceof Error ? e.message : String(e);
		} finally {
			view.busy = false;
		}
	}

	async function pollJobUntilDone(name: string): Promise<void> {
		// Only re-fetch the JobRuns projection — paths and templates
		// don't change while a backup runs.
		while (true) {
			await new Promise((r) => setTimeout(r, 2000));
			try {
				const runs = await listJobRuns();
				const lastRunByJob = mapLastRun(runs);
				const fresh = lastRunByJob.get(name) ?? null;
				const view = views.find((v) => v.job.name === name);
				if (!view) return;
				view.lastRun = fresh;
				if (!fresh || fresh.status !== 'running') return;
			} catch {
				// Network hiccup: keep polling — the user will see the
				// stale state until the next round succeeds.
			}
		}
	}

	async function deleteJob(name: string) {
		if (
			!confirm(
				`Delete job "${name}"? Snapshots / mirror files on the repository are kept; only the kovre.yaml entry is removed.`
			)
		)
			return;
		const view = views.find((v) => v.job.name === name);
		if (!view) return;
		view.busy = true;
		try {
			const cfg = await getConfig();
			const yaml = emitConfigYaml(removeJob(cfg.parsed, name));
			await putConfig(yaml);
			views = views.filter((v) => v.job.name !== name);
		} catch (e) {
			view.actionMessage = e instanceof Error ? e.message : String(e);
			view.busy = false;
		}
	}

	// ---- Derived ---------------------------------------------------------

	const lastSuccessfulRun = $derived.by(() => {
		let best: JobRun | null = null;
		for (const v of views) {
			const r = v.lastRun;
			if (r && r.status === 'success' && (!best || r.finished_at! > best.finished_at!)) {
				best = r;
			}
		}
		return best;
	});

	const totalProtectedPaths = $derived(
		views.reduce((sum, v) => sum + (v.resolved?.paths.length ?? 0), 0)
	);

	const usedTemplates = $derived(
		new Set(
			views
				.map((v) => v.job.template)
				.filter((t): t is string => typeof t === 'string')
				.map((t) => (t === 'documents' ? 'user-files' : t)) // normalize alias
		)
	);

	const viewsByCategory = $derived.by(() => {
		const map = new Map<string, JobView[]>();
		for (const cat of CATEGORIES) map.set(cat.id, []);
		for (const v of views) {
			const cid = categoryOf(v.job);
			(map.get(cid) ?? map.get('other')!).push(v);
		}
		return map;
	});

	const suggestedTemplates = $derived.by(() => {
		// Templates the user has *not* used yet, in their catalog order,
		// excluding the `custom` escape hatch (which isn't a discoverable
		// thing — it's just "I'll fill in the paths myself").
		return templates.filter(
			(t) => t.name !== 'custom' && !usedTemplates.has(t.name)
		);
	});

	// ---- Health classification ------------------------------------------
	//
	// Per-job traffic light. Soft thresholds — what matters is "is the
	// last run recent enough that I should feel covered?".
	const runningJobs = $derived(
		views.filter((v) => healthKind(v) === 'running').map((v) => v.job.name)
	);
	const okCount = $derived(views.filter((v) => healthKind(v) === 'ok').length);
	const warnCount = $derived(
		views.filter((v) => ['stale', 'failed'].includes(healthKind(v))).length
	);
	const neverCount = $derived(views.filter((v) => healthKind(v) === 'never').length);

	const unreachableRepos = $derived(
		Object.entries(repoStatus)
			.filter(([, s]) => !s.reachable)
			.map(([name]) => name)
	);

	const allOk = $derived(
		views.length > 0 && warnCount === 0 && neverCount === 0 && unreachableRepos.length === 0
	);

	const STALE_AFTER_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

	function healthKind(view: JobView): 'ok' | 'stale' | 'failed' | 'running' | 'never' {
		const r = view.lastRun;
		if (!r) return 'never';
		if (r.status === 'running') return 'running';
		if (r.status === 'failed') return 'failed';
		if (r.status === 'success') {
			if (!r.finished_at) return 'ok';
			const ago = Date.now() - Date.parse(r.finished_at);
			return ago > STALE_AFTER_MS ? 'stale' : 'ok';
		}
		return 'never';
	}

	function healthLabel(kind: ReturnType<typeof healthKind>): string {
		switch (kind) {
			case 'ok':
				return 'covered';
			case 'stale':
				return 'stale (>7d)';
			case 'failed':
				return 'failed';
			case 'running':
				return 'running';
			case 'never':
				return 'never run';
		}
	}
</script>

<div class="inventory">
	<header class="hero" class:hero-ok={!loading && !error && allOk} class:hero-warn={!loading && !error && (warnCount > 0 || neverCount > 0)}>
		{#if loading}
			<div class="hero-signal">⟳</div>
			<h2>Checking your backups…</h2>
		{:else if error}
			<div class="hero-signal signal-error">✗</div>
			<h2>Something went wrong</h2>
			<p class="hero-sub">{error}</p>
		{:else if views.length === 0}
			<div class="hero-signal signal-neutral">○</div>
			<h2>No backups configured yet</h2>
			<p class="hero-sub"><a href="/templates">Pick a template to get started →</a></p>
		{:else if allOk}
			<div class="hero-signal signal-ok">✓</div>
			<div>
				<h2>Your data is safe</h2>
				<p class="hero-sub">
					{views.length} job{views.length === 1 ? '' : 's'} · {totalProtectedPaths} path{totalProtectedPaths === 1 ? '' : 's'} protected · last backup {formatRelative(lastSuccessfulRun?.finished_at ?? lastSuccessfulRun?.started_at ?? '')}
				</p>
				{#if runningJobs.length > 0}
					<p class="hero-sub hero-running">⟳ Running: {runningJobs.join(', ')}</p>
				{/if}
			</div>
		{:else}
			<div class="hero-signal signal-warn">!</div>
			<div>
				<h2>
					{#if unreachableRepos.length > 0}
						{unreachableRepos.length} backup destination{unreachableRepos.length === 1 ? ' is' : 's are'} offline
					{:else if warnCount > 0}
						{warnCount} job{warnCount === 1 ? '' : 's'} need{warnCount === 1 ? 's' : ''} attention
					{:else}
						{neverCount} job{neverCount === 1 ? ' has' : 's have'} never run
					{/if}
				</h2>
				{#if unreachableRepos.length > 0}
					<p class="hero-sub hero-alert">
						Unreachable: {unreachableRepos.join(', ')} — is the NAS / external drive connected?
					</p>
				{/if}
				<p class="hero-sub">
					{okCount} protected · {views.length} total · {totalProtectedPaths} path{totalProtectedPaths === 1 ? '' : 's'}
				</p>
			</div>
		{/if}
	</header>

	{#if !loading && !error}
	<div class="category-grid">
		{#each CATEGORIES as cat (cat.id)}
			{@const items = viewsByCategory.get(cat.id) ?? []}
			{#if items.length > 0}
				<section class="category">
					<h3>
						<span class="cat-icon">{cat.icon}</span>
						{cat.label}
						<span class="cat-count">{items.length} job{items.length === 1 ? '' : 's'}</span>
					</h3>
					<p class="cat-desc">{cat.description}</p>

					<ul class="jobs">
						{#each items as v (v.job.name)}
							{@const kind = healthKind(v)}
							<li class={`job kind-${kind}`}>
								<div class="job-row-1">
									<span class={`status-dot kind-${kind}`}></span>
									<a class="job-name" href={`/jobs/${encodeURIComponent(v.job.name)}`}>
										{v.job.name}
									</a>
									<span class={`health-label kind-${kind}`}>{healthLabel(kind)}</span>
									<span class="job-when">
										{v.lastRun ? formatRelative(v.lastRun.finished_at ?? v.lastRun.started_at) : ''}
									</span>
									{#if v.lastRun?.bytes_processed != null}
										<span class="job-size">{formatBytes(v.lastRun.bytes_processed)}</span>
									{/if}
								</div>

								<div class="job-row-2">
									<span class="job-detail">
										{#if v.resolved && v.resolved.paths.length > 0}
											{v.resolved.paths.length} path{v.resolved.paths.length === 1 ? '' : 's'} → {v.job.repository}
										{:else if v.resolved && v.resolved.paths.length === 0}
											<span class="warn-text">No paths on this machine</span>
										{:else}
											→ {v.job.repository}
										{/if}
										{#if v.repoBackend}
											<span class={`badge-sm badge-${v.repoBackend}`}>{v.repoBackend}</span>
										{/if}
									</span>
									<div class="job-actions">
										<button
											type="button"
											class="btn-sm btn-primary"
											disabled={v.busy || v.lastRun?.status === 'running'}
											onclick={() => runJob(v.job.name)}
										>
											{v.busy || v.lastRun?.status === 'running' ? '⟳' : '▶'} Run
										</button>
										<a class="btn-sm btn-ghost" href={`/jobs/${encodeURIComponent(v.job.name)}/restore`}>
											↻ Restore
										</a>
										<a class="btn-sm btn-ghost" href={`/jobs/${encodeURIComponent(v.job.name)}/edit`}>
											Edit
										</a>
										<button
											type="button"
											class="btn-sm btn-danger"
											disabled={v.busy || v.lastRun?.status === 'running'}
											onclick={() => deleteJob(v.job.name)}
										>
											×
										</button>
									</div>
								</div>

								{#if v.actionMessage}
									<p class="action-msg">{v.actionMessage}</p>
								{/if}
								{#if v.lastRun?.failure_reason}
									<p class="failure-banner">⚠ {v.lastRun.failure_reason}</p>
								{/if}
							</li>
						{/each}
					</ul>
				</section>
			{/if}
		{/each}
	</div>

		{#if suggestedTemplates.length > 0}
			<section class="suggestions">
				<h3>You don't back this up yet</h3>
				<p class="cat-desc">
					One click pre-fills the wizard. Templates that don't apply to your
					machine resolve to an empty list of paths — no harm done.
				</p>
				<div class="suggestion-row">
					{#each suggestedTemplates as t (t.name)}
						<a class="suggest" href={`/templates/${encodeURIComponent(t.name)}`}>
							<span class="s-icon">{t.icon}</span>
							<span class="s-label">{t.name}</span>
						</a>
					{/each}
				</div>
			</section>
		{/if}

		{#if views.length === 0}
			<section class="empty-state">
				<p>
					No jobs declared in <code>kovre.yaml</code> yet.
					<a href="/templates">Pick a template →</a>
				</p>
			</section>
		{/if}
	{/if}
</div>

<style>
	.inventory {
		display: flex;
		flex-direction: column;
		gap: 1.6rem;
	}

	/* ---- Hero "health dashboard" ---- */
	.hero {
		display: flex;
		align-items: center;
		gap: 1.2rem;
		padding: 1.6rem 2rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 12px;
		transition: background 0.3s, border-color 0.3s;
	}
	.hero-ok {
		background: var(--ok-bg);
		border-color: var(--ok-border);
	}
	.hero-warn {
		background: var(--warn-bg);
		border-color: var(--warn-border);
	}
	.hero-signal {
		font-size: 2.4rem;
		line-height: 1;
		width: 3rem;
		height: 3rem;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 50%;
		background: var(--surface-raised);
		color: var(--text-muted);
		flex-shrink: 0;
	}
	.signal-ok {
		background: var(--ok-border);
		color: var(--ok);
	}
	.signal-warn {
		background: var(--warn-border);
		color: var(--warn);
	}
	.signal-error {
		background: var(--error-bg);
		color: var(--error);
	}
	.signal-neutral {
		background: var(--surface-raised);
		color: var(--text-muted);
	}
	.hero h2 {
		margin: 0;
		font-size: 1.3rem;
		font-weight: 600;
		color: var(--text-primary);
	}
	.hero-sub {
		margin: 0.2rem 0 0;
		color: var(--text-secondary);
		font-size: 0.92rem;
	}
	.hero-sub a {
		color: var(--accent);
		text-decoration: none;
	}
	.hero-sub a:hover {
		text-decoration: underline;
	}
	.hero-alert {
		color: var(--warn);
		font-weight: 500;
	}
	.hero-running {
		color: var(--accent);
	}
	.muted {
		color: var(--text-muted);
		font-size: 0.9rem;
		margin: 0.3rem 0 0;
	}
	.error {
		color: var(--error);
		margin: 0.3rem 0 0;
	}

	/* ---- Categories grid ---- */
	.category-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(440px, 1fr));
		gap: 1.2rem;
	}
	.category {
		padding: 1.1rem 1.4rem 1.3rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 10px;
		display: flex;
		flex-direction: column;
	}
	.category h3 {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin: 0 0 0.25rem;
		font-size: 1rem;
		font-weight: 600;
		color: var(--text-primary);
	}
	.cat-icon {
		font-size: 1.3rem;
		line-height: 1;
	}
	.cat-count {
		margin-left: auto;
		font-size: 0.75rem;
		color: var(--text-secondary);
		background: var(--surface-raised);
		padding: 0.1rem 0.5rem;
		border-radius: 10px;
	}
	.cat-desc {
		margin: 0 0 0.8rem;
		color: var(--text-muted);
		font-size: 0.82rem;
	}

	/* ---- Job cards (simplified) ---- */
	.jobs {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.job {
		padding: 0.55rem 0.75rem;
		background: var(--surface-raised);
		border: 1px solid var(--border);
		border-radius: 8px;
		border-left: 3px solid var(--text-muted);
	}
	.job.kind-ok { border-left-color: var(--ok); }
	.job.kind-stale { border-left-color: var(--warn); }
	.job.kind-failed { border-left-color: var(--error); }
	.job.kind-running { border-left-color: var(--accent); }
	.job.kind-never { border-left-color: var(--text-muted); }

	.job-row-1 {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}
	.status-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		flex-shrink: 0;
		background: var(--text-muted);
	}
	.status-dot.kind-ok { background: var(--ok); }
	.status-dot.kind-stale { background: var(--warn); }
	.status-dot.kind-failed { background: var(--error); }
	.status-dot.kind-running {
		background: var(--accent);
		animation: pulse 1.2s ease-in-out infinite;
	}
	@keyframes pulse {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.5; transform: scale(1.4); }
	}

	.job-name {
		color: var(--text-primary);
		font-weight: 500;
		text-decoration: none;
		font-size: 0.95rem;
	}
	.job-name:hover { color: var(--accent); }

	.health-label {
		font-size: 0.75rem;
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.03em;
	}
	.health-label.kind-ok { color: var(--ok); }
	.health-label.kind-stale { color: var(--warn); }
	.health-label.kind-failed { color: var(--error); }
	.health-label.kind-running { color: var(--accent); }
	.health-label.kind-never { color: var(--text-muted); }

	.job-when {
		color: var(--text-muted);
		font-size: 0.78rem;
	}
	.job-size {
		color: var(--text-secondary);
		font-size: 0.78rem;
	}

	.job-row-2 {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-top: 0.3rem;
	}
	.job-detail {
		color: var(--text-secondary);
		font-size: 0.8rem;
	}
	.warn-text { color: var(--warn); }

	.badge-sm {
		display: inline-block;
		padding: 0.05rem 0.35rem;
		border-radius: 3px;
		font-size: 0.65rem;
		text-transform: uppercase;
		letter-spacing: 0.03em;
		vertical-align: middle;
		margin-left: 0.3rem;
	}
	.badge-rustic {
		background: var(--accent-bg);
		color: var(--accent);
		border: 1px solid var(--accent-border);
	}
	.badge-mirror {
		background: var(--ok-bg);
		color: var(--ok);
		border: 1px solid var(--ok-border);
	}

	.job-actions {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 0.25rem;
	}

	/* ---- Buttons (design-system) ---- */
	.btn-sm {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		padding: 0.22rem 0.55rem;
		border: 1px solid var(--border);
		border-radius: 5px;
		font: inherit;
		font-size: 0.75rem;
		font-weight: 500;
		cursor: pointer;
		text-decoration: none;
		transition: background 0.15s, color 0.15s;
	}
	.btn-primary {
		background: var(--accent-bg);
		color: var(--accent);
		border-color: var(--accent-border);
	}
	.btn-primary:hover:not(:disabled) {
		background: var(--accent-border);
	}
	.btn-ghost {
		background: transparent;
		color: var(--text-secondary);
	}
	.btn-ghost:hover {
		background: var(--surface-raised);
		color: var(--text-primary);
	}
	.btn-danger {
		background: transparent;
		color: var(--text-muted);
	}
	.btn-danger:hover:not(:disabled) {
		background: var(--error-bg);
		color: var(--error);
	}
	.btn-sm:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}

	.action-msg {
		margin: 0.3rem 0 0;
		padding: 0.25rem 0.5rem;
		background: var(--accent-bg);
		border-radius: 4px;
		font-size: 0.78rem;
		color: var(--accent);
	}
	.failure-banner {
		margin: 0.35rem 0 0;
		padding: 0.3rem 0.6rem;
		background: var(--error-bg);
		border: 1px solid var(--error);
		border-radius: 5px;
		color: var(--error);
		font-size: 0.78rem;
		overflow-wrap: anywhere;
	}

	/* ---- Suggestions ---- */
	.suggestions {
		padding: 1.2rem 1.4rem;
		background: var(--surface);
		border: 1px dashed var(--accent-border);
		border-radius: 10px;
	}
	.suggestions h3 {
		margin: 0 0 0.3rem;
		font-size: 0.95rem;
		font-weight: 500;
		color: var(--text-primary);
	}
	.suggestion-row {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	.suggest {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		padding: 0.4rem 0.75rem;
		background: var(--accent-bg);
		border: 1px solid var(--accent-border);
		border-radius: 6px;
		color: var(--accent);
		text-decoration: none;
		font-size: 0.85rem;
		transition: background 0.15s;
	}
	.suggest:hover {
		background: var(--accent-border);
	}
	.s-icon { font-size: 1rem; }
	.s-label { font-size: 0.82rem; }

	.empty-state {
		padding: 2rem;
		text-align: center;
		color: var(--text-secondary);
	}
	.empty-state a {
		color: var(--accent);
		text-decoration: none;
	}
</style>
