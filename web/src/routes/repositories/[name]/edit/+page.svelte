<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { getConfig, putConfig, type ConfigPayload } from '$lib/api';
	import { emitConfigYaml, updateRepository, type RepositoryDraft } from '$lib/yaml';
	import RepoForm from '$lib/RepoForm.svelte';

	const repoName = $derived(page.params.name ?? '');

	let config = $state<ConfigPayload | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let busy = $state(false);
	let submitError = $state<string | null>(null);

	let draft = $state<RepositoryDraft>({
		name: '',
		path: '',
		backend: 'rustic',
		password_file: ''
	});

	onMount(async () => {
		try {
			const cfg = await getConfig();
			config = cfg;
			const existing = cfg.parsed.repositories[repoName];
			if (!existing) {
				loadError = `No repository named "${repoName}" in kovre.yaml`;
				return;
			}
			draft = {
				name: repoName,
				path: existing.path,
				backend: existing.backend ?? 'rustic',
				password_file: existing.password_file ?? ''
			};
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	async function onSubmit(d: RepositoryDraft) {
		if (!config) return;
		if (d.name !== repoName && d.name in config.parsed.repositories) {
			submitError = `a repository named "${d.name}" already exists`;
			return;
		}
		busy = true;
		submitError = null;
		try {
			const yaml = emitConfigYaml(updateRepository(config.parsed, repoName, d));
			await putConfig(yaml);
			goto('/repositories');
		} catch (e) {
			submitError = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<a class="back" href="/repositories">← repositories</a>

<h2>Edit repository: {repoName}</h2>

{#if loading}
	<p>Loading…</p>
{:else if loadError}
	<p class="error">{loadError}</p>
{:else}
	<p class="hint">
		Renaming this repository will also rewrite every <code>repository:</code> reference
		in <code>kovre.yaml</code>, so existing jobs keep working.
	</p>
	{#if submitError}
		<p class="error">{submitError}</p>
	{/if}
	<RepoForm bind:draft {busy} submitLabel="Save changes" onsubmit={onSubmit} oncancel={() => goto('/repositories')} />
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
		margin: 0 0 0.75rem;
		font-size: 1.25rem;
		font-weight: 500;
		color: #e6e8eb;
	}
	.hint {
		color: #9aa3b2;
		font-size: 0.9rem;
		max-width: 640px;
		margin: 0 0 1.25rem;
	}
	.hint code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #c5cad3;
	}
	.error {
		color: #f47373;
		margin: 0 0 1rem;
	}
</style>
