<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getConfig,
		listJobs,
		listJobRuns,
		listTemplates,
		putConfig,
		resolveTemplate,
		triggerRun,
		type Job,
		type JobRun,
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
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [jobs, runs, tmpls, cfg] = await Promise.all([
				listJobs(),
				listJobRuns(),
				listTemplates(),
				getConfig()
			]);
			templates = tmpls;
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
	<header class="hero">
		<h2>Your machine</h2>
		{#if loading}
			<p class="muted">Inventorying…</p>
		{:else if error}
			<p class="error">Error: {error}</p>
		{:else}
			<p class="hero-line">
				{#if lastSuccessfulRun}
					Last successful backup: <strong>{formatRelative(lastSuccessfulRun.finished_at ?? lastSuccessfulRun.started_at)}</strong>
					({lastSuccessfulRun.job_name})
				{:else}
					<span class="warn">No successful backup yet.</span>
				{/if}
				· {views.length} job{views.length === 1 ? '' : 's'} configured · {totalProtectedPaths} path{totalProtectedPaths === 1 ? '' : 's'} watched
			</p>
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
								<div class="job-head">
									<a class="job-name" href={`/jobs/${encodeURIComponent(v.job.name)}`}>
										{v.job.name}
									</a>
									<span class={`health kind-${kind}`}>{healthLabel(kind)}</span>
									<span class="when">
										{v.lastRun
											? formatRelative(v.lastRun.finished_at ?? v.lastRun.started_at)
											: '—'}
									</span>
									<div class="job-actions">
										<button
											type="button"
											class="action-run"
											disabled={v.busy || v.lastRun?.status === 'running'}
											onclick={() => runJob(v.job.name)}
											title="Run this job now"
										>
											{v.busy || v.lastRun?.status === 'running' ? '⟳' : '▶'} Run
										</button>
										<a
											class="action-restore"
											href={`/jobs/${encodeURIComponent(v.job.name)}/restore`}
											title="Restore this job's content to a destination folder"
										>
											↻
										</a>
										<a
											class="action-edit"
											href={`/jobs/${encodeURIComponent(v.job.name)}/edit`}
											title="Edit"
										>
											✎
										</a>
										<button
											type="button"
											class="action-del"
											disabled={v.busy || v.lastRun?.status === 'running'}
											onclick={() => deleteJob(v.job.name)}
											title="Delete this job (kovre.yaml only; data on disk is kept)"
										>
											×
										</button>
									</div>
								</div>
								{#if v.actionMessage}
									<p class="action-msg">{v.actionMessage}</p>
								{/if}
								<div class="flow">
									<div class="source">
										{#if v.resolveError}
											<p class="resolve-error">Could not resolve paths: {v.resolveError}</p>
										{:else if v.resolved && v.resolved.paths.length > 0}
											<ul class="paths">
												{#each v.resolved.paths as p}
													<li class="path">{p}</li>
												{/each}
											</ul>
										{:else if v.resolved && v.resolved.paths.length === 0}
											<p class="empty-paths">
												Template resolves to <strong>no paths</strong> on this machine —
												nothing to back up here yet.
											</p>
										{:else if v.job.template}
											<p class="muted">
												(resolved at run time by template "{v.job.template}")
											</p>
										{/if}
									</div>

									<div class="arrow" aria-hidden="true">→</div>

									<div class="dest">
										<a href="/repositories" class="dest-name" title="Repository">
											📦 {v.job.repository}
										</a>
										<div class="dest-meta">
											{#if v.repoBackend}
												<span class={`backend-badge backend-${v.repoBackend}`}>
													{v.repoBackend}
												</span>
											{/if}
											{#if v.lastRun?.bytes_processed != null}
												<span class="dest-stat" title="Bytes covered by the last successful run">
													{formatBytes(v.lastRun.bytes_processed)}
												</span>
											{/if}
											{#if retentionSummary(v.job)}
												<span class="dest-stat" title="Retention policy">
													♻ {retentionSummary(v.job)}
												</span>
											{/if}
										</div>
									</div>
								</div>

								{#if v.lastRun?.failure_reason}
									<p class="failure-banner" title="Last failure reason">
										⚠ {v.lastRun.failure_reason}
									</p>
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
		gap: 2rem;
	}

	.hero {
		padding: 1.4rem 1.6rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 8px;
	}
	.hero h2 {
		margin: 0 0 0.4rem;
		font-size: 1.2rem;
		font-weight: 500;
		color: #c5cad3;
	}
	.hero-line {
		margin: 0;
		color: #9aa3b2;
		font-size: 0.95rem;
	}
	.hero-line strong {
		color: #e6e8eb;
		font-weight: 500;
	}
	.hero-line .warn {
		color: #f5d36a;
	}
	.muted {
		color: #6a7180;
		font-size: 0.9rem;
		margin: 0.3rem 0 0;
	}
	.error {
		color: #f47373;
		margin: 0.3rem 0 0;
	}

	.category-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(440px, 1fr));
		gap: 1.2rem;
	}
	.category {
		padding: 1.3rem 1.6rem 1.5rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 8px;
		display: flex;
		flex-direction: column;
	}
	.category h3 {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		margin: 0 0 0.3rem;
		font-size: 1.05rem;
		font-weight: 500;
		color: #e6e8eb;
	}
	.cat-icon {
		font-size: 1.4rem;
		line-height: 1;
	}
	.cat-count {
		margin-left: auto;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.78rem;
		color: #80a8e6;
		background: #1d2a3f;
		border: 1px solid #2a4d8f;
		padding: 0.1rem 0.5rem;
		border-radius: 3px;
	}
	.cat-desc {
		margin: 0 0 1rem;
		color: #6a7180;
		font-size: 0.85rem;
	}

	.jobs {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.job {
		padding: 0.6rem 0.8rem;
		background: #1a1f27;
		border: 1px solid #2a2f38;
		border-radius: 5px;
		border-left-width: 3px;
	}
	.job.kind-ok {
		border-left-color: #2a8857;
	}
	.job.kind-stale {
		border-left-color: #a07a1a;
	}
	.job.kind-failed {
		border-left-color: #a02a2a;
	}
	.job.kind-running {
		border-left-color: #355fb0;
	}
	.job.kind-never {
		border-left-color: #4a4f58;
	}

	.job-head {
		display: flex;
		align-items: baseline;
		gap: 0.7rem;
		margin-bottom: 0.35rem;
	}
	.job-name {
		color: #e6e8eb;
		font-weight: 500;
		text-decoration: none;
		font-size: 0.98rem;
	}
	.job-name:hover {
		color: #80a8e6;
	}

	.health {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.72rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 0.12rem 0.5rem;
		border-radius: 3px;
	}
	.health.kind-ok {
		background: #1f3a2c;
		color: #6ad08e;
	}
	.health.kind-stale {
		background: #3a341f;
		color: #f5d36a;
	}
	.health.kind-failed {
		background: #3a1f1f;
		color: #f47373;
	}
	.health.kind-running {
		background: #1d2a3f;
		color: #80a8e6;
	}
	.health.kind-never {
		background: #1f242c;
		color: #9aa3b2;
	}

	.when {
		color: #6a7180;
		font-size: 0.82rem;
	}

	.job-actions {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 0.3rem;
	}
	.action-run {
		padding: 0.25rem 0.6rem;
		background: #1d2a3f;
		color: #80a8e6;
		border: 1px solid #2a4d8f;
		border-radius: 3px;
		font: inherit;
		font-size: 0.78rem;
		font-weight: 500;
		cursor: pointer;
	}
	.action-run:hover:not(:disabled) {
		background: #243551;
		color: #a8c4f0;
	}
	.action-run:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.action-restore,
	.action-edit,
	.action-del {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.6rem;
		height: 1.6rem;
		background: transparent;
		color: #6a7180;
		border: 1px solid #2a2f38;
		border-radius: 3px;
		font: inherit;
		font-size: 0.9rem;
		text-decoration: none;
		cursor: pointer;
	}
	.action-edit:hover {
		color: #c5cad3;
		background: #1f242c;
	}
	.action-restore:hover {
		color: #80a8e6;
		border-color: #2a4d8f;
		background: #1d2a3f;
	}
	.action-del:hover:not(:disabled) {
		color: #f47373;
		border-color: #5a2a2a;
		background: #2a1f1f;
	}
	.action-del:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}

	.action-msg {
		margin: 0.3rem 0 0;
		padding: 0.3rem 0.55rem;
		background: #1f242c;
		border-radius: 3px;
		font-size: 0.8rem;
		color: #80a8e6;
	}

	.flow {
		display: grid;
		grid-template-columns: 1fr auto minmax(140px, auto);
		align-items: center;
		gap: 0.6rem;
	}
	.source {
		min-width: 0;
	}
	.arrow {
		font-size: 1.4rem;
		color: #4a5564;
		line-height: 1;
		user-select: none;
	}
	.dest {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 0.4rem 0.6rem;
		background: #131720;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		text-align: right;
		min-width: 140px;
	}
	.dest-name {
		color: #e6e8eb;
		text-decoration: none;
		font-weight: 500;
		font-size: 0.88rem;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.dest-name:hover {
		color: #80a8e6;
	}
	.dest-meta {
		display: flex;
		flex-direction: column;
		gap: 0.18rem;
		align-items: flex-end;
	}
	.dest-stat {
		color: #9aa3b2;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.72rem;
	}
	.backend-badge {
		display: inline-block;
		padding: 0.05rem 0.4rem;
		border-radius: 3px;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.68rem;
		text-transform: uppercase;
		letter-spacing: 0.03em;
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

	.failure-banner {
		margin: 0.5rem 0 0;
		padding: 0.4rem 0.7rem;
		background: #2a1f1f;
		border: 1px solid #5a2a2a;
		border-radius: 4px;
		color: #f47373;
		font-size: 0.82rem;
		overflow-wrap: anywhere;
	}

	.paths {
		margin: 0;
		padding-left: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
	}
	.path {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.82rem;
		color: #c5cad3;
		overflow-wrap: anywhere;
	}
	.path::before {
		content: '› ';
		color: #4a4f58;
	}
	.empty-paths {
		margin: 0;
		color: #9aa3b2;
		font-size: 0.85rem;
	}
	.empty-paths strong {
		color: #f5d36a;
	}
	.resolve-error {
		margin: 0;
		color: #f47373;
		font-size: 0.85rem;
	}

	.suggestions {
		padding: 1.3rem 1.6rem;
		background: #131720;
		border: 1px dashed #2a4d8f;
		border-radius: 8px;
	}
	.suggestions h3 {
		margin: 0 0 0.3rem;
		font-size: 1rem;
		font-weight: 500;
		color: #c5cad3;
	}
	.suggestion-row {
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
	}
	.suggest {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.45rem 0.85rem;
		background: #1d2a3f;
		border: 1px solid #2a4d8f;
		border-radius: 4px;
		color: #80a8e6;
		text-decoration: none;
		font-size: 0.9rem;
	}
	.suggest:hover {
		background: #243551;
		color: #a8c4f0;
	}
	.s-icon {
		font-size: 1rem;
	}
	.s-label {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
	}

	.empty-state {
		padding: 1.5rem;
		text-align: center;
		color: #9aa3b2;
	}
	.empty-state code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #c5cad3;
	}
	.empty-state a {
		color: #80a8e6;
		text-decoration: none;
	}
</style>
