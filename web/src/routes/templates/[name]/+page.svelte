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
	import { addJob, emitConfigYaml, type JobDraft } from '$lib/yaml';
	import DirInput from '$lib/DirInput.svelte';

	const templateName = $derived(page.params.name ?? '');

	let template = $state<Template | null>(null);
	let config = $state<ConfigPayload | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	let jobName = $state('');
	let repository = $state('');
	let optionValues = $state<Record<string, unknown>>({});
	let retention = $state<Record<string, number | ''>>({
		keep_last: '',
		keep_daily: '',
		keep_weekly: '',
		keep_monthly: ''
	});

	let submitting = $state(false);
	let submitMessage = $state<string | null>(null);
	let submitError = $state<string | null>(null);

	onMount(async () => {
		try {
			const [tmpls, cfg] = await Promise.all([listTemplates(), getConfig()]);
			template = tmpls.find((t) => t.name === templateName) ?? null;
			config = cfg;
			// Default the new job's name to the template name + auto-suffix
			// if a job with that name already exists.
			const existing = new Set(Object.keys(cfg.parsed.jobs ?? {}));
			let candidate = templateName;
			let i = 2;
			while (existing.has(candidate)) {
				candidate = `${templateName}-${i++}`;
			}
			jobName = candidate;
			// Default to the first repository declared.
			repository = Object.keys(cfg.parsed.repositories ?? {})[0] ?? '';
			// Initialize option values.
			for (const opt of template?.options ?? []) {
				optionValues[opt.key] =
					opt.type === 'directory_list' || opt.type === 'string_list' ? [''] : '';
			}
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	function addRow(key: string) {
		const cur = (optionValues[key] as string[]) ?? [];
		optionValues[key] = [...cur, ''];
	}
	function removeRow(key: string, idx: number) {
		const cur = ((optionValues[key] as string[]) ?? []).slice();
		cur.splice(idx, 1);
		optionValues[key] = cur.length > 0 ? cur : [''];
	}
	function updateRow(key: string, idx: number, value: string) {
		const cur = ((optionValues[key] as string[]) ?? []).slice();
		cur[idx] = value;
		optionValues[key] = cur;
	}

	function buildDraft(): JobDraft {
		const draft: JobDraft = {
			name: jobName.trim(),
			repository: repository.trim(),
		};

		if (template?.name === 'custom') {
			const paths = ((optionValues.paths as string[]) ?? [])
				.map((s) => s.trim())
				.filter((s) => s.length > 0);
			draft.paths = paths;

			const excludes = ((optionValues.excludes as string[]) ?? [])
				.map((s) => s.trim())
				.filter((s) => s.length > 0);
			if (excludes.length > 0) draft.excludes = excludes;
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
		submitting = true;
		submitMessage = null;
		submitError = null;
		try {
			const draft = buildDraft();
			if (!draft.name) throw new Error('job name is required');
			if (!draft.repository) throw new Error('repository is required');
			if (config.parsed.jobs && draft.name in config.parsed.jobs) {
				throw new Error(`a job named "${draft.name}" already exists`);
			}
			const yaml = emitConfigYaml(addJob(config.parsed, draft));
			await putConfig(yaml);
			submitMessage = `Saved. New job "${draft.name}" added.`;
			// Navigate back to overview after a beat so the user sees feedback.
			setTimeout(() => goto('/'), 400);
		} catch (e) {
			submitError = e instanceof Error ? e.message : String(e);
		} finally {
			submitting = false;
		}
	}
</script>

<a class="back" href="/templates">← templates</a>

{#if loading}
	<p>Loading…</p>
{:else if loadError}
	<p class="error">Error: {loadError}</p>
{:else if !template}
	<p class="error">Unknown template "{templateName}".</p>
{:else}
	<h2>
		<span class="icon">{template.icon}</span>
		new {template.name} job
	</h2>
	<p class="lead">{template.description}</p>

	<form
		onsubmit={(e) => {
			e.preventDefault();
			submit();
		}}
	>
		<label>
			<span class="label">Job name</span>
			<input type="text" bind:value={jobName} required />
			<span class="hint">
				A short identifier — what you'll click on in the overview.
			</span>
		</label>

		<label>
			<span class="label">Repository</span>
			<select bind:value={repository} required>
				{#each Object.keys(config?.parsed.repositories ?? {}) as r (r)}
					<option value={r}>{r}</option>
				{/each}
			</select>
			<span class="hint">
				One of the repositories declared in <code>repositories:</code>.
			</span>
		</label>

		{#each template.options as opt (opt.key)}
			<fieldset>
				<legend>{opt.label}{opt.required ? ' *' : ''}</legend>

				{#if opt.type === 'directory'}
					<DirInput bind:value={optionValues[opt.key] as string} placeholder="C:\..." />

				{:else if opt.type === 'directory_list'}
					{#each (optionValues[opt.key] as string[]) as _, idx ((opt.key, idx))}
						<div class="row">
							<DirInput
								value={(optionValues[opt.key] as string[])[idx]}
								onchange={(v) => updateRow(opt.key, idx, v)}
								placeholder="C:\..."
							/>
							<button type="button" class="row-remove" onclick={() => removeRow(opt.key, idx)}>
								remove
							</button>
						</div>
					{/each}
					<button type="button" class="row-add" onclick={() => addRow(opt.key)}>
						+ add folder
					</button>

				{:else if opt.type === 'string_list'}
					{#each (optionValues[opt.key] as string[]) as _, idx ((opt.key, idx))}
						<div class="row">
							<input
								type="text"
								value={(optionValues[opt.key] as string[])[idx]}
								oninput={(e) => updateRow(opt.key, idx, (e.target as HTMLInputElement).value)}
								placeholder="**/*.tmp"
							/>
							<button type="button" class="row-remove" onclick={() => removeRow(opt.key, idx)}>
								remove
							</button>
						</div>
					{/each}
					<button type="button" class="row-add" onclick={() => addRow(opt.key)}>
						+ add pattern
					</button>
				{/if}
			</fieldset>
		{/each}

		<fieldset>
			<legend>Retention (optional)</legend>
			<div class="retention">
				{#each ['keep_last', 'keep_daily', 'keep_weekly', 'keep_monthly'] as k}
					<label class="retention-row">
						<span class="retention-label">{k.replace('keep_', 'keep last/by ')}</span>
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

		{#if submitMessage}
			<p class="success">{submitMessage}</p>
		{/if}
		{#if submitError}
			<p class="error">{submitError}</p>
		{/if}

		<div class="actions">
			<button type="submit" class="submit" disabled={submitting}>
				{submitting ? 'saving…' : 'Save job'}
			</button>
			<a href="/templates" class="cancel">cancel</a>
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
	.icon {
		font-size: 1.5rem;
		margin-right: 0.4rem;
	}
	.lead {
		color: #9aa3b2;
		margin: 0 0 1.5rem;
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
	.hint {
		color: #6a7180;
		font-size: 0.8rem;
	}
	.hint code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #9aa3b2;
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
		align-items: stretch;
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
		align-items: center;
		gap: 0.75rem;
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

	.success {
		color: #6ad08e;
	}
	.error {
		color: #f47373;
	}
</style>
