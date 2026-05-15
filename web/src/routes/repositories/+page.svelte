<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import {
		getConfig,
		getRepositoriesStatus,
		initRepository,
		putConfig,
		verifyRepository,
		type ConfigPayload,
		type RepositoryStatus
	} from '$lib/api';
	import {
		emitConfigYaml,
		jobsUsingRepository,
		removeRepository
	} from '$lib/yaml';

	let config = $state<ConfigPayload | null>(null);
	let status = $state<Record<string, RepositoryStatus>>({});
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busyName = $state<string | null>(null);
	let banner = $state<string | null>(null);
	let bannerKind = $state<'info' | 'ok' | 'warn'>('info');

	onMount(async () => {
		try {
			const [cfg, st] = await Promise.all([getConfig(), getRepositoriesStatus()]);
			config = cfg;
			status = st;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	async function refreshStatus() {
		try {
			status = await getRepositoriesStatus();
		} catch {
			// Non-blocking. The table simply keeps the previous status
			// snapshot until the next render.
		}
	}

	async function onInit(name: string) {
		busyName = name;
		banner = null;
		try {
			const res = await initRepository(name);
			banner = res.justInitialized
				? `Initialized repository "${name}".`
				: `Repository "${name}" is already initialized — no action needed.`;
			bannerKind = 'info';
			await refreshStatus();
		} catch (e) {
			banner = e instanceof Error ? e.message : String(e);
			bannerKind = 'warn';
		} finally {
			busyName = null;
		}
	}

	async function onVerify(name: string) {
		busyName = name;
		banner = null;
		try {
			const res = await verifyRepository(name);
			const trailer = res.messages.length > 0 ? ` — ${res.messages.join('; ')}` : '';
			banner = res.ok
				? `Verified "${name}": no integrity errors found.${trailer}`
				: `Verified "${name}": findings reported.${trailer}`;
			bannerKind = res.ok ? 'ok' : 'warn';
		} catch (e) {
			banner = `Verify "${name}" failed: ${e instanceof Error ? e.message : String(e)}`;
			bannerKind = 'warn';
		} finally {
			busyName = null;
		}
	}

	async function onDelete(name: string) {
		if (!config) return;
		const dependents = jobsUsingRepository(config.parsed, name);
		if (dependents.length > 0) {
			banner = `cannot delete: jobs reference this repository — ${dependents.join(', ')}`;
			bannerKind = 'warn';
			return;
		}
		if (!confirm(`Delete repository "${name}"? The data on disk is NOT touched.`)) return;

		busyName = name;
		banner = null;
		try {
			const yaml = emitConfigYaml(removeRepository(config.parsed, name));
			const fresh = await putConfig(yaml);
			config = fresh;
			banner = `Deleted repository "${name}".`;
			bannerKind = 'info';
		} catch (e) {
			banner = e instanceof Error ? e.message : String(e);
			bannerKind = 'warn';
		} finally {
			busyName = null;
		}
	}
</script>

<div class="header">
	<h2>Repositories</h2>
	<a class="new" href="/repositories/new">+ New repository</a>
</div>

<p class="lead">
	A repository is a storage destination plus its backend kind. <strong>rustic</strong> is
	encrypted + deduplicated (best for offsite/dev). <strong>mirror</strong> writes plain files
	with overwritten/deleted ones archived to a sibling <code>.versions/</code> (best for
	photos, documents, anything you want to browse in Explorer).
</p>

{#if banner}
	<p class="banner" class:banner-ok={bannerKind === 'ok'} class:banner-warn={bannerKind === 'warn'}>
		{banner}
	</p>
{/if}

{#if loading}
	<p>Loading…</p>
{:else if error}
	<p class="error">Error: {error}</p>
{:else if !config || Object.keys(config.parsed.repositories).length === 0}
	<p class="empty">
		No repositories declared yet. <a href="/repositories/new">Add one →</a>
	</p>
{:else}
	<table>
		<thead>
			<tr>
				<th>Name</th>
				<th>Backend</th>
				<th>Path</th>
				<th>Password file</th>
				<th>Used by</th>
				<th></th>
			</tr>
		</thead>
		<tbody>
			{#each Object.entries(config.parsed.repositories) as [name, repo] (name)}
				{@const dependents = jobsUsingRepository(config.parsed, name)}
				{@const backendKind = repo.backend ?? 'rustic'}
				<tr>
					<td class="name">{name}</td>
					<td>
						<span class="badge badge-{backendKind}">{backendKind}</span>
					</td>
					<td class="mono">{repo.path}</td>
					<td class="mono">{repo.password_file ?? '—'}</td>
					<td>
						{#if dependents.length === 0}
							<span class="muted">—</span>
						{:else}
							{dependents.join(', ')}
						{/if}
					</td>
					<td class="actions">
						{#if status[name]?.initialized}
							<span class="ready" title="Destination is ready on disk">✓ initialized</span>
						{:else}
							<button
								type="button"
								class="init"
								disabled={busyName === name}
								onclick={() => onInit(name)}
								title="Materialize the repository on disk"
							>
								{busyName === name ? '…' : 'init'}
							</button>
						{/if}
						<button
							type="button"
							class="verify"
							disabled={busyName === name}
							onclick={() => onVerify(name)}
							title={backendKind === 'mirror'
								? 'No-op for mirror — files are native on disk'
								: 'Walk metadata + index for corruption (rustic check)'}
						>
							{busyName === name ? '…' : 'verify'}
						</button>
						<a class="edit" href={`/repositories/${encodeURIComponent(name)}/edit`}>edit</a>
						<button
							type="button"
							class="delete"
							disabled={busyName === name || dependents.length > 0}
							onclick={() => onDelete(name)}
							title={dependents.length > 0 ? `Used by ${dependents.length} job(s)` : ''}
						>
							{busyName === name ? '…' : 'delete'}
						</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

<style>
	.header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin: 0 0 0.5rem;
	}
	h2 {
		margin: 0;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}
	.new {
		padding: 0.4rem 0.8rem;
		background: #2a4d8f;
		color: #e6e8eb;
		border-radius: 4px;
		text-decoration: none;
		font-size: 0.9rem;
	}
	.new:hover {
		background: #355fb0;
	}

	.lead {
		color: #9aa3b2;
		max-width: 640px;
		margin: 0 0 1.5rem;
	}

	.banner {
		padding: 0.55rem 0.8rem;
		background: #1f242c;
		border-radius: 4px;
		color: #80a8e6;
		font-size: 0.95rem;
		margin: 0 0 1rem;
		border-left: 3px solid #355fb0;
	}
	.banner-ok {
		color: #6ad08e;
		border-left-color: #2a8857;
	}
	.banner-warn {
		color: #f5d36a;
		border-left-color: #a07a1a;
	}

	.badge {
		display: inline-block;
		padding: 0.15rem 0.5rem;
		border-radius: 3px;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.78rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	.badge-rustic {
		background: #1d2a3f;
		color: #80a8e6;
		border: 1px solid #2a4d8f;
	}
	.badge-mirror {
		background: #1f3a2c;
		color: #6ad08e;
		border: 1px solid #2a4d3f;
	}

	.verify {
		padding: 0.3rem 0.7rem;
		background: #1f242c;
		color: #c5cad3;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		font: inherit;
		font-size: 0.85rem;
		cursor: pointer;
	}
	.verify:hover:not(:disabled) {
		background: #262c36;
		color: #e6e8eb;
	}
	.verify:disabled {
		opacity: 0.5;
		cursor: not-allowed;
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
		padding: 0.55rem 0.75rem;
		border-bottom: 1px solid #1a1d24;
	}
	tbody tr:hover td {
		background: #161a21;
	}
	.name {
		color: #e6e8eb;
		font-weight: 500;
	}
	.mono {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
	}
	.muted {
		color: #6a7180;
	}
	.actions {
		display: flex;
		gap: 0.5rem;
		justify-content: flex-end;
	}
	.ready {
		display: inline-block;
		padding: 0.25rem 0.65rem;
		color: #6ad08e;
		font-size: 0.82rem;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
	}

	.init {
		padding: 0.3rem 0.7rem;
		background: #1f3a2c;
		color: #6ad08e;
		border: 1px solid #2a4d3f;
		border-radius: 4px;
		font: inherit;
		font-size: 0.85rem;
		cursor: pointer;
	}
	.init:hover:not(:disabled) {
		background: #244736;
		color: #8ce0a8;
	}
	.init:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.edit {
		padding: 0.3rem 0.7rem;
		background: #1f242c;
		border-radius: 4px;
		color: #c5cad3;
		text-decoration: none;
		font-size: 0.85rem;
	}
	.edit:hover {
		background: #262c36;
		color: #e6e8eb;
	}
	.delete {
		padding: 0.3rem 0.7rem;
		background: #2a1f1f;
		color: #f47373;
		border: 1px solid #3a2a2a;
		border-radius: 4px;
		font: inherit;
		font-size: 0.85rem;
		cursor: pointer;
	}
	.delete:hover:not(:disabled) {
		background: #3a1f1f;
		color: #ff8a8a;
	}
	.delete:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.error {
		color: #f47373;
	}
	.empty {
		color: #9aa3b2;
	}
	.empty a {
		color: #80a8e6;
	}
</style>
