<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { getConfig, initRepository, putConfig, type ConfigPayload } from '$lib/api';
	import { addRepository, emitConfigYaml, type RepositoryDraft } from '$lib/yaml';
	import RepoForm from '$lib/RepoForm.svelte';

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
			config = await getConfig();
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	});

	async function onSubmit(d: RepositoryDraft) {
		if (!config) return;
		if (d.name in config.parsed.repositories) {
			submitError = `a repository named "${d.name}" already exists`;
			return;
		}
		busy = true;
		submitError = null;
		try {
			const yaml = emitConfigYaml(addRepository(config.parsed, d));
			await putConfig(yaml);
			// Auto-init so the first backup doesn't fail with
			// "No repository config file found" (rustic) or "destination
			// does not exist" (mirror). If the rustic path already had
			// a repo (existing NAS backup folder), the server returns
			// 409 and initRepository treats that as a benign no-op.
			try {
				await initRepository(d.name);
			} catch (initErr) {
				const label = d.backend === 'mirror' ? 'mirror init' : 'rustic init';
				submitError = `Config saved but ${label} failed: ${initErr instanceof Error ? initErr.message : String(initErr)}. You can retry from the repositories list.`;
				return;
			}
			goto('/repositories');
		} catch (e) {
			submitError = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}
</script>

<a class="back" href="/repositories">← repositories</a>

<h2>New repository</h2>

{#if loading}
	<p>Loading…</p>
{:else if loadError}
	<p class="error">Error: {loadError}</p>
{:else}
	{#if submitError}
		<p class="error">{submitError}</p>
	{/if}
	<RepoForm bind:draft {busy} submitLabel="Save repository" onsubmit={onSubmit} oncancel={() => goto('/repositories')} />
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
		margin: 0 0 1rem;
		font-size: 1.25rem;
		font-weight: 500;
		color: #e6e8eb;
	}
	.error {
		color: #f47373;
		margin: 0 0 1rem;
	}
</style>
