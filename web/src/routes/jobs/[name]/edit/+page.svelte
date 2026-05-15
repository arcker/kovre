<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		getConfig,
		listTemplates,
		putConfig,
		type ConfigPayload,
		type Template
	} from '$lib/api';
	import {
		emitConfigYaml,
		updateJob,
		type JobDraft,
		type JobEntry
	} from '$lib/yaml';
	import DirInput from '$lib/DirInput.svelte';

	const jobName = $derived(page.params.name ?? '');

	let template = $state<Template | null>(null);
	let templates = $state<Template[]>([]);
	let config = $state<ConfigPayload | null>(null);
	let originalJob = $state<JobEntry | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	let jobNameField = $state('');
	let repository = $state('');
	let optionValues = $state<Record<string, unknown>>({});
	let customPaths = $state<string[]>(['']);
	let customExcludes = $state<string[]>(['']);
	let retention = $state<Record<string, number | ''>>({
		keep_last: '',
		keep_daily: '',
		keep_weekly: '',
		keep_monthly: '',
		keep_versions: ''
	});

	// Which backend the currently-selected repository uses. Drives
	// the retention UI: rustic exposes keep_last/daily/weekly/monthly,
	// mirror exposes keep_versions only.
	const selectedBackend = $derived(
		(config?.parsed.repositories?.[repository]?.backend ?? 'rustic') as 'rustic' | 'mirror'
	);
	const retentionKeys = $derived(
		selectedBackend === 'mirror'
			? (['keep_versions'] as const)
			: (['keep_last', 'keep_daily', 'keep_weekly', 'keep_monthly'] as const)
	);

	let busy = $state(false);
	let submitError = $state<string | null>(null);

	onMount(async () => {
		try {
			const [tmpls, cfg] = await Promise.all([listTemplates(), getConfig()]);
			templates = tmpls;
			config = cfg;
			const existing = cfg.parsed.jobs[jobName] as JobEntry | undefined;
			if (!existing) {
				loadError = `No job named "${jobName}" in kovre.yaml`;
				return;
			}
			originalJob = existing;
			jobNameField = jobName;
			repository = existing.repository;

			if (existing.template) {
				template = tmpls.find((t) => t.name === existing.template) ?? null;
				const opts = (existing.template_options ?? {}) as Record<string, unknown>;
				for (const o of template?.options ?? []) {
					optionValues[o.key] =
						o.type === 'directory_list' || o.type === 'string_list'
							? Array.isArray(opts[o.key]) ? (opts[o.key] as string[]).slice() : ['']
							: (opts[o.key] ?? '');
				}
			} else {
				// Custom job — use the special "custom" pseudo-template.
				template = tmpls.find((t) => t.name === 'custom') ?? null;
				customPaths = existing.paths && existing.paths.length > 0 ? existing.paths.slice() : [''];
				customExcludes = existing.excludes && existing.excludes.length > 0 ? existing.excludes.slice() : [''];
			}

			const ret = existing.retention ?? {};
			for (const k of Object.keys(retention)) {
				const v = (ret as Record<string, number | null | undefined>)[k];
				retention[k] = v == null ? '' : v;
			}
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	function addRow(target: 'paths' | 'excludes') {
		if (target === 'paths') customPaths = [...customPaths, ''];
		else customExcludes = [...customExcludes, ''];
	}
	function removeRow(target: 'paths' | 'excludes', idx: number) {
		const arr = (target === 'paths' ? customPaths : customExcludes).slice();
		arr.splice(idx, 1);
		const next = arr.length > 0 ? arr : [''];
		if (target === 'paths') customPaths = next;
		else customExcludes = next;
	}
	function updateRow(target: 'paths' | 'excludes', idx: number, value: string) {
		const arr = (target === 'paths' ? customPaths : customExcludes).slice();
		arr[idx] = value;
		if (target === 'paths') customPaths = arr;
		else customExcludes = arr;
	}

	function buildDraft(): JobDraft {
		const draft: JobDraft = {
			name: jobNameField.trim(),
			repository: repository.trim()
		};

		if (template?.name === 'custom' || (!originalJob?.template && template == null)) {
			const paths = customPaths.map((s) => s.trim()).filter((s) => s.length > 0);
			draft.paths = paths;
			const excl = customExcludes.map((s) => s.trim()).filter((s) => s.length > 0);
			if (excl.length > 0) draft.excludes = excl;
		} else {
			draft.template = template?.name ?? null;
			const opts: Record<string, unknown> = {};
			for (const opt of template?.options ?? []) {
				const v = optionValues[opt.key];
				if (v != null && v !== '') opts[opt.key] = v;
			}
			if (Object.keys(opts).length > 0) draft.template_options = opts;
		}

		const ret: Record<string, number> = {};
		for (const [k, v] of Object.entries(retention)) {
			if (v !== '' && v != null) {
				const n = Number(v);
				if (Number.isFinite(n) && n > 0) ret[k] = n;
			}
		}
		if (Object.keys(ret).length > 0) draft.retention = ret;

		return draft;
	}

	async function submit() {
		if (!config) return;
		busy = true;
		submitError = null;
		try {
			const draft = buildDraft();
			if (!draft.name) throw new Error('job name is required');
			if (!draft.repository) throw new Error('repository is required');
			if (draft.name !== jobName && draft.name in config.parsed.jobs) {
				throw new Error(`a job named "${draft.name}" already exists`);
			}
			const yaml = emitConfigYaml(updateJob(config.parsed, jobName, draft));
			await putConfig(yaml);
			goto(`/jobs/${encodeURIComponent(draft.name)}`);
		} catch (e) {
			submitError = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<a class="back" href={`/jobs/${jobName}`}>← back to {jobName}</a>

{#if loading}
	<p>Loading…</p>
{:else if loadError}
	<p class="error">{loadError}</p>
{:else if !originalJob}
	<p class="error">Job not found.</p>
{:else}
	<h2>Edit job: {jobName}</h2>
	<p class="lead">
		Template: <code>{originalJob.template ?? 'custom'}</code>. Renaming the job will
		drop its run history (the events are keyed by the old name).
	</p>

	<form
		onsubmit={(e) => {
			e.preventDefault();
			submit();
		}}
	>
		<label>
			<span class="label">Job name</span>
			<input type="text" bind:value={jobNameField} required />
		</label>

		<label>
			<span class="label">Repository</span>
			<select bind:value={repository} required>
				{#each Object.keys(config?.parsed.repositories ?? {}) as r (r)}
					<option value={r}>{r}</option>
				{/each}
			</select>
		</label>

		{#if template?.name === 'custom' || (!originalJob.template)}
			<fieldset>
				<legend>Paths *</legend>
				{#each customPaths as _, idx (idx)}
					<div class="row">
						<DirInput
							value={customPaths[idx]}
							onchange={(v) => updateRow('paths', idx, v)}
							placeholder="C:\..."
						/>
						<button type="button" class="row-remove" onclick={() => removeRow('paths', idx)}>
							remove
						</button>
					</div>
				{/each}
				<button type="button" class="row-add" onclick={() => addRow('paths')}>
					+ add folder
				</button>
			</fieldset>

			<fieldset>
				<legend>Exclude patterns (glob)</legend>
				{#each customExcludes as _, idx (idx)}
					<div class="row">
						<input
							type="text"
							value={customExcludes[idx]}
							oninput={(e) =>
								updateRow('excludes', idx, (e.target as HTMLInputElement).value)}
							placeholder="**/*.tmp"
						/>
						<button
							type="button"
							class="row-remove"
							onclick={() => removeRow('excludes', idx)}
						>
							remove
						</button>
					</div>
				{/each}
				<button type="button" class="row-add" onclick={() => addRow('excludes')}>
					+ add pattern
				</button>
			</fieldset>
		{:else if template}
			{#each template.options as opt (opt.key)}
				<fieldset>
					<legend>{opt.label}{opt.required ? ' *' : ''}</legend>
					{#if opt.type === 'directory'}
						<DirInput bind:value={optionValues[opt.key] as string} placeholder="C:\..." />
					{/if}
				</fieldset>
			{/each}
		{/if}

		<fieldset>
			<legend>Retention</legend>
			<p class="retention-hint">
				{#if selectedBackend === 'mirror'}
					Mirror keeps the current canonical state plus archived versions of overwritten/deleted
					files under <code>.versions/</code>. <code>keep_versions</code> caps how many of those
					archived versions are kept per file.
				{:else}
					Rustic snapshots are independent. Each <code>keep_*</code> rule retains the most recent
					matching snapshots; the others are forgotten after each backup.
				{/if}
			</p>
			<div class="retention">
				{#each retentionKeys as k (k)}
					<label class="retention-row">
						<span class="retention-label">{k.replace('keep_', 'keep ')}</span>
						<input
							type="number"
							min="1"
							bind:value={retention[k]}
							placeholder="0 = off"
						/>
					</label>
				{/each}
			</div>
		</fieldset>

		{#if submitError}
			<p class="error">{submitError}</p>
		{/if}

		<div class="actions">
			<button type="submit" class="submit" disabled={busy}>
				{busy ? 'saving…' : 'Save changes'}
			</button>
			<a href={`/jobs/${jobName}`} class="cancel">cancel</a>
		</div>
	</form>
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
		margin: 0 0 0.5rem;
		font-size: 1.25rem;
		font-weight: 500;
		color: #e6e8eb;
	}
	.lead {
		color: #9aa3b2;
		margin: 0 0 1.25rem;
	}
	.lead code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #c5cad3;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 600px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.label {
		color: #c5cad3;
		font-size: 0.9rem;
		font-weight: 500;
	}

	input[type='text'],
	input[type='number'],
	select {
		padding: 0.5rem 0.75rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		color: #e6e8eb;
		font: inherit;
		font-size: 0.95rem;
	}
	input[type='text']:focus,
	input[type='number']:focus,
	select:focus {
		outline: none;
		border-color: #355fb0;
	}

	fieldset {
		border: 1px solid #2a2f38;
		border-radius: 5px;
		padding: 1rem 1.1rem 1rem;
	}
	legend {
		padding: 0 0.5rem;
		color: #9aa3b2;
		font-size: 0.85rem;
	}

	.row {
		display: flex;
		gap: 0.5rem;
		margin-bottom: 0.5rem;
	}
	.row > :global(.dir-input) {
		flex: 1;
	}
	.row > input[type='text'] {
		flex: 1;
	}
	.row-remove,
	.row-add {
		padding: 0.4rem 0.7rem;
		background: #1f242c;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		color: #9aa3b2;
		font: inherit;
		font-size: 0.85rem;
		cursor: pointer;
	}
	.row-remove:hover,
	.row-add:hover {
		background: #262c36;
		color: #e6e8eb;
	}

	.retention-hint {
		color: #9aa3b2;
		font-size: 0.85rem;
		margin: 0 0 0.7rem;
	}
	.retention-hint code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #c5cad3;
	}
	.retention {
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		gap: 0.5rem 0.8rem;
	}
	.retention-row {
		flex-direction: row;
		align-items: center;
		gap: 0.5rem;
	}
	.retention-label {
		flex: 1;
		color: #c5cad3;
		font-size: 0.85rem;
	}
	.retention-row input {
		width: 6rem;
	}

	.actions {
		display: flex;
		gap: 0.75rem;
		align-items: center;
	}
	.submit {
		padding: 0.55rem 1.2rem;
		background: #2a4d8f;
		color: #e6e8eb;
		border: none;
		border-radius: 4px;
		cursor: pointer;
		font: inherit;
		font-weight: 500;
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
	.error {
		color: #f47373;
	}
</style>
