<script lang="ts">
	import { onMount } from 'svelte';

	interface ServerInfo {
		server: string;
		version: string;
		timestamp: string;
		endpoints: Record<string, string>;
		models: string[];
	}

	let info = $state<ServerInfo | null>(null);
	let healthy = $state<boolean | null>(null);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [infoResp, healthResp] = await Promise.all([fetch('/info'), fetch('/health')]);
			info = await infoResp.json();
			healthy = healthResp.ok;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	});
</script>

<h2>About</h2>

<p>
	kovre dashboard — Phase 2.<br />
	Backup orchestrator for Windows, declarative YAML, rustic_core engine.
</p>

{#if error}
	<p class="error">Could not reach the server: {error}</p>
{:else if info}
	<dl>
		<dt>Server</dt>
		<dd>{info.server} {info.version}</dd>
		<dt>Health</dt>
		<dd class={healthy ? 'ok' : 'bad'}>{healthy ? 'ok' : 'unhealthy'}</dd>
		<dt>Server time</dt>
		<dd class="mono">{info.timestamp}</dd>
		<dt>Models</dt>
		<dd>
			{#if info.models.length === 0}
				<span class="muted">(none reported)</span>
			{:else}
				{info.models.join(', ')}
			{/if}
		</dd>
		<dt>Endpoints</dt>
		<dd>
			<ul>
				{#each Object.entries(info.endpoints) as [name, path]}
					<li><span class="mono">{path}</span> — {name}</li>
				{/each}
			</ul>
		</dd>
	</dl>
{:else}
	<p>Loading…</p>
{/if}

<style>
	h2 {
		margin: 0 0 1.25rem;
		font-size: 1.1rem;
		font-weight: 500;
		color: #c5cad3;
	}

	p {
		font-size: 0.95rem;
		color: #c5cad3;
	}

	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.4rem 1rem;
		padding: 0.75rem 1rem;
		background: #161a21;
		border-radius: 4px;
	}
	dt {
		color: #9aa3b2;
		font-size: 0.85rem;
	}
	dd {
		margin: 0;
		font-size: 0.9rem;
	}

	ul {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	ul li {
		padding: 0.1rem 0;
	}

	.ok {
		color: #6ad08e;
	}
	.bad {
		color: #f47373;
	}
	.muted {
		color: #6a7180;
	}
	.mono {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
	}
	.error {
		color: #f47373;
	}
</style>
