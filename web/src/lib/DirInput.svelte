<script lang="ts">
	import { listFs } from '$lib/api';

	interface Props {
		value: string;
		placeholder?: string;
		onchange?: (v: string) => void;
	}

	let { value = $bindable(''), placeholder = 'C:\\Users\\...', onchange }: Props = $props();

	let suggestions = $state<string[]>([]);
	let open = $state(false);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let debounceHandle: ReturnType<typeof setTimeout> | null = null;

	// Reactive: every time `value` changes, refresh suggestions (debounced).
	// We list the parent of the current text — if the user has typed
	// `C:\Users\yoa` we list `C:\Users\` and the dropdown shows entries
	// that start with `yoa`.
	function refresh(text: string) {
		if (debounceHandle) clearTimeout(debounceHandle);
		debounceHandle = setTimeout(async () => {
			const parent = parentOf(text);
			if (!parent) {
				suggestions = [];
				return;
			}
			loading = true;
			error = null;
			try {
				const listing = await listFs(parent);
				const prefix = leafOf(text).toLowerCase();
				suggestions = listing.entries
					.filter((e) => e.is_dir && e.name.toLowerCase().startsWith(prefix))
					.map((e) => join(parent, e.name));
			} catch (e) {
				error = e instanceof Error ? e.message : String(e);
				suggestions = [];
			} finally {
				loading = false;
			}
		}, 200);
	}

	function parentOf(text: string): string {
		const norm = text.replace(/\//g, '\\');
		const idx = norm.lastIndexOf('\\');
		if (idx < 0) return '';
		// Preserve trailing slash on drive roots ("C:\")
		if (idx === 2 && norm[1] === ':') return norm.slice(0, 3);
		return norm.slice(0, idx);
	}

	function leafOf(text: string): string {
		const norm = text.replace(/\//g, '\\');
		const idx = norm.lastIndexOf('\\');
		return idx < 0 ? norm : norm.slice(idx + 1);
	}

	function join(parent: string, leaf: string): string {
		if (parent.endsWith('\\')) return `${parent}${leaf}`;
		return `${parent}\\${leaf}`;
	}

	function onInput(e: Event) {
		const v = (e.target as HTMLInputElement).value;
		value = v;
		onchange?.(v);
		refresh(v);
		open = true;
	}

	function pick(s: string) {
		value = s;
		onchange?.(s);
		open = false;
	}
</script>

<div class="dir-input">
	<input
		type="text"
		bind:value
		oninput={onInput}
		onfocus={() => {
			refresh(value);
			open = true;
		}}
		onblur={() => setTimeout(() => (open = false), 150)}
		{placeholder}
		spellcheck="false"
	/>
	{#if open && (suggestions.length > 0 || loading || error)}
		<ul class="dropdown">
			{#if loading}
				<li class="muted">listing…</li>
			{:else if error}
				<li class="error">{error}</li>
			{:else}
				{#each suggestions.slice(0, 12) as s (s)}
					<li>
						<button type="button" onmousedown={() => pick(s)}>{s}</button>
					</li>
				{/each}
			{/if}
		</ul>
	{/if}
</div>

<style>
	.dir-input {
		position: relative;
	}
	input {
		width: 100%;
		padding: 0.5rem 0.75rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		color: #e6e8eb;
		font: inherit;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.9rem;
	}
	input:focus {
		outline: none;
		border-color: #355fb0;
	}
	.dropdown {
		position: absolute;
		top: 100%;
		left: 0;
		right: 0;
		margin: 0.2rem 0 0;
		padding: 0.3rem 0;
		list-style: none;
		background: #1f242c;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		max-height: 240px;
		overflow-y: auto;
		z-index: 10;
	}
	.dropdown li {
		padding: 0;
	}
	.dropdown button {
		width: 100%;
		text-align: left;
		padding: 0.35rem 0.75rem;
		background: none;
		border: none;
		color: #c5cad3;
		font: inherit;
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		font-size: 0.85rem;
		cursor: pointer;
	}
	.dropdown button:hover {
		background: #262c36;
		color: #e6e8eb;
	}
	.muted {
		padding: 0.35rem 0.75rem;
		color: #6a7180;
		font-size: 0.85rem;
	}
	.error {
		padding: 0.35rem 0.75rem;
		color: #f47373;
		font-size: 0.85rem;
	}
</style>
