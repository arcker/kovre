<script lang="ts">
	import DirInput from '$lib/DirInput.svelte';
	import { fsStat, initRepositoryPassword } from '$lib/api';
	import type { RepositoryDraft } from '$lib/yaml';

	interface Props {
		draft: RepositoryDraft;
		busy?: boolean;
		submitLabel: string;
		onsubmit: (d: RepositoryDraft) => void;
		oncancel?: () => void;
	}

	let { draft = $bindable(), busy = false, submitLabel, onsubmit, oncancel }: Props = $props();

	let nameError = $state<string | null>(null);
	let pathError = $state<string | null>(null);
	let passwordError = $state<string | null>(null);
	let passwordWarning = $state<string | null>(null);
	let generateMessage = $state<string | null>(null);
	let generating = $state(false);

	async function checkPasswordFile() {
		passwordWarning = null;
		const p = draft.password_file.trim();
		if (p === '') return;
		try {
			const stat = await fsStat(p);
			if (!stat.exists) {
				passwordWarning =
					'this file does not exist yet — create it manually with the passphrase inside, or click "Generate" below to have kovre create one for you.';
			} else if (stat.is_dir) {
				passwordWarning = 'this path points at a directory — pick a file path instead.';
			}
		} catch {
			// Network error / server hiccup is not blocking — let the user save anyway.
		}
	}

	async function onGenerate() {
		generateMessage = null;
		const p = draft.password_file.trim();
		if (p === '') {
			passwordError =
				'enter a target path first (e.g. C:\\ProgramData\\Kovre\\<repo>.key), then click Generate.';
			return;
		}
		generating = true;
		try {
			const res = await initRepositoryPassword(p);
			generateMessage = `wrote a ${res.length}-character passphrase to ${res.path}. The passphrase content never leaves the kovre process; review the file's ACLs to lock down access.`;
			passwordWarning = null;
			passwordError = null;
		} catch (e) {
			passwordError = e instanceof Error ? e.message : String(e);
		} finally {
			generating = false;
		}
	}

	function submit() {
		nameError = draft.name.trim() === '' ? 'name is required' : null;
		pathError = draft.path.trim() === '' ? 'path is required' : null;
		passwordError = draft.password_file.trim() === '' ? 'password file is required' : null;
		if (nameError || pathError || passwordError) return;
		onsubmit({
			name: draft.name.trim(),
			path: draft.path.trim(),
			password_file: draft.password_file.trim()
		});
	}
</script>

<form
	onsubmit={(e) => {
		e.preventDefault();
		submit();
	}}
>
	<label>
		<span class="label">Name</span>
		<input type="text" bind:value={draft.name} placeholder="nas, local-drive, …" />
		<span class="hint">
			Short identifier referenced by jobs under <code>repository:</code>.
		</span>
		{#if nameError}
			<span class="error">{nameError}</span>
		{/if}
	</label>

	<label>
		<span class="label">Path</span>
		<DirInput bind:value={draft.path} placeholder="\\nas.local\backup\kovre or D:\Backups" />
		<span class="hint">
			Filesystem path or UNC share where rustic stores blobs / index / snapshots.
		</span>
		{#if pathError}
			<span class="error">{pathError}</span>
		{/if}
	</label>

	<label>
		<span class="label">Password file</span>
		<div class="password-row">
			<input
				type="text"
				bind:value={draft.password_file}
				onblur={checkPasswordFile}
				placeholder="C:\ProgramData\Kovre\nas.key"
			/>
			<button type="button" class="gen" onclick={onGenerate} disabled={generating}>
				{generating ? 'generating…' : 'Generate'}
			</button>
		</div>
		<div class="hint password-hint">
			<p>
				A plain-text file kovre reads at backup time to unlock the rustic
				repository. <strong>It must exist on disk before the first
				<code>init-repo</code> or backup.</strong>
			</p>
			<ul>
				<li>
					Format: the file's first line is the passphrase. Whitespace and trailing
					newlines are stripped.
				</li>
				<li>
					Lock the ACLs (NTFS) so only your Windows user can read it — kovre
					doesn't manage permissions for you.
				</li>
				<li>
					<strong>"Generate" creates the file with a 256-bit random passphrase
					at the path above.</strong> The content never travels through your
					browser. Keep a copy in your password manager — losing it means
					losing the repository.
				</li>
			</ul>
		</div>
		{#if passwordError}
			<span class="error">{passwordError}</span>
		{/if}
		{#if passwordWarning}
			<span class="warning">{passwordWarning}</span>
		{/if}
		{#if generateMessage}
			<span class="success">{generateMessage}</span>
		{/if}
	</label>

	<div class="actions">
		<button type="submit" class="submit" disabled={busy}>
			{busy ? 'saving…' : submitLabel}
		</button>
		{#if oncancel}
			<button type="button" class="cancel" onclick={oncancel}>cancel</button>
		{/if}
	</div>
</form>

<style>
	form {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		max-width: 640px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
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
	.hint code {
		font-family: ui-monospace, 'Cascadia Mono', Menlo, monospace;
		color: #9aa3b2;
	}
	.password-hint p {
		margin: 0 0 0.4rem;
	}
	.password-hint ul {
		margin: 0;
		padding-left: 1.1rem;
	}
	.password-hint li {
		margin: 0.15rem 0;
	}
	.password-hint strong {
		color: #c5cad3;
	}

	input[type='text'] {
		padding: 0.5rem 0.75rem;
		background: #161a21;
		border: 1px solid #2a2f38;
		border-radius: 4px;
		color: #e6e8eb;
		font: inherit;
		font-size: 0.95rem;
		width: 100%;
		box-sizing: border-box;
	}
	input[type='text']:focus {
		outline: none;
		border-color: #355fb0;
	}

	.password-row {
		display: flex;
		gap: 0.5rem;
		align-items: stretch;
	}
	.password-row input {
		flex: 1;
	}
	.gen {
		padding: 0.4rem 0.95rem;
		background: #1f242c;
		border: 1px solid #2a4d8f;
		border-radius: 4px;
		color: #80a8e6;
		font: inherit;
		font-size: 0.9rem;
		cursor: pointer;
		white-space: nowrap;
	}
	.gen:hover:not(:disabled) {
		background: #262c36;
		color: #a8c4f0;
	}
	.gen:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.error {
		color: #f47373;
		font-size: 0.85rem;
	}
	.warning {
		color: #f5d36a;
		font-size: 0.85rem;
	}
	.success {
		color: #6ad08e;
		font-size: 0.85rem;
	}

	.actions {
		display: flex;
		gap: 0.75rem;
		align-items: center;
		margin-top: 0.5rem;
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
		padding: 0.55rem 0.9rem;
		background: none;
		color: #9aa3b2;
		border: none;
		font: inherit;
		font-size: 0.9rem;
		cursor: pointer;
	}
	.cancel:hover {
		color: #e6e8eb;
	}
</style>
