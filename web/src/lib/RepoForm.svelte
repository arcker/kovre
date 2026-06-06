<script lang="ts">
	import DirInput from '$lib/DirInput.svelte';
	import { fsStat, initRepositoryPassword, storeSmbPassword } from '$lib/api';
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

	// SMB local state — the password itself is never bound to `draft`,
	// it lives in this component only for the duration of the "Store"
	// click and is dropped immediately after.
	let smbPassword = $state('');
	let smbStoring = $state(false);
	let smbMessage = $state<string | null>(null);
	let smbError = $state<string | null>(null);

	const isUnc = $derived(draft.path.trim().startsWith('\\\\'));

	async function onStoreSmbPassword() {
		smbMessage = null;
		smbError = null;
		const target = draft.smb_password_file.trim();
		if (target === '') {
			smbError =
				'enter a target path first for the encrypted blob (e.g. C:\\ProgramData\\Kovre\\<repo>.smb.dpapi).';
			return;
		}
		if (smbPassword === '') {
			smbError = 'enter the SMB password before clicking Store.';
			return;
		}
		smbStoring = true;
		try {
			const res = await storeSmbPassword(target, smbPassword);
			smbMessage = `Encrypted blob written to ${res.path}. The plaintext password never touched disk. Lock the file's ACLs to your user only.`;
			smbPassword = ''; // zero out from memory ASAP
		} catch (e) {
			smbError = e instanceof Error ? e.message : String(e);
		} finally {
			smbStoring = false;
		}
	}

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
		// Password file is required for rustic only.
		passwordError =
			draft.backend === 'rustic' && draft.password_file.trim() === ''
				? 'password file is required for the rustic backend'
				: null;
		if (nameError || pathError || passwordError) return;
		onsubmit({
			name: draft.name.trim(),
			path: draft.path.trim(),
			backend: draft.backend,
			password_file: draft.backend === 'rustic' ? draft.password_file.trim() : '',
			smb_user: isUnc ? draft.smb_user.trim() : '',
			smb_password_file: isUnc ? draft.smb_password_file.trim() : ''
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
		<span class="label">Backend</span>
		<select bind:value={draft.backend}>
			<option value="mirror">mirror — plain files + .versions/ (browsable, no passphrase) — recommended</option>
			<option value="rustic">rustic — deduplicated, encrypted (for dev/logs/dumps)</option>
		</select>
		<span class="hint">
			<strong>mirror</strong> is the default: files are written 1:1 to the destination so you can
			browse them straight from Explorer; overwritten and deleted files move to a sibling
			<code>.versions/</code> folder. Best for photos, documents, mails, game saves — everything
			you'll want to recover by hand one day.
			<br />
			<strong>rustic</strong> is restic-compatible: encrypted, deduplicated, snapshot-based.
			Worth it for dev trees, log archives, database dumps — places where dedup pays off and you
			don't need plain-Explorer access. Requires a password file.
		</span>
	</label>

	<label>
		<span class="label">Path</span>
		<DirInput bind:value={draft.path} placeholder="\\nas.local\backup\kovre or D:\Backups" />
		<span class="hint">
			{#if draft.backend === 'mirror'}
				Filesystem path or UNC share where mirrored files will land. Inside this folder
				kovre creates one subdirectory per job, plus a sibling <code>.versions/</code>.
			{:else}
				Filesystem path or UNC share where rustic stores blobs / index / snapshots.
			{/if}
		</span>
		{#if pathError}
			<span class="error">{pathError}</span>
		{/if}
	</label>

	{#if draft.backend === 'rustic'}
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
	{:else}
	<p class="mirror-note">
		Mirror backend: no passphrase needed — files are written natively. Make sure the destination
		folder's ACLs already restrict access to your Windows user (kovre doesn't manage permissions).
	</p>
	{/if}

	{#if isUnc}
		<fieldset class="smb-section">
			<legend>SMB authentication (UNC share detected)</legend>
			<p class="smb-intro">
				The path is a network share. If your Windows session is already authenticated against it
				(via Credential Manager or a previous mapping), leave this section empty. Otherwise, kovre
				can authenticate at boot with the credentials below.
			</p>

			<label>
				<span class="label">SMB user</span>
				<input type="text" bind:value={draft.smb_user} placeholder="kovre-backup or DOMAIN\\user" />
			</label>

			<label>
				<span class="label">SMB password file (DPAPI blob)</span>
				<input
					type="text"
					bind:value={draft.smb_password_file}
					placeholder="C:\ProgramData\Kovre\nas.smb.dpapi"
				/>
				<span class="hint">
					Encrypted with Windows DPAPI (CurrentUser scope). Only your user on this machine can
					decrypt it. The plaintext password never touches disk; <code>kovre.yaml</code> only stores
					the path.
				</span>
			</label>

			<label>
				<span class="label">Set SMB password (one-shot)</span>
				<div class="smb-row">
					<input
						type="password"
						bind:value={smbPassword}
						placeholder="never written to YAML"
						autocomplete="off"
					/>
					<button type="button" class="gen" onclick={onStoreSmbPassword} disabled={smbStoring}>
						{smbStoring ? 'encrypting…' : 'Store'}
					</button>
				</div>
				<span class="hint">
					Encrypts the password via DPAPI and writes the resulting blob to the file path above.
					Lock that file's ACLs to your user only after — kovre doesn't do it for you.
				</span>
			</label>

			{#if smbError}
				<span class="error">{smbError}</span>
			{/if}
			{#if smbMessage}
				<span class="success">{smbMessage}</span>
			{/if}
		</fieldset>
	{/if}

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

	.smb-section {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		padding: 1rem 1.1rem;
		background: #1f242c;
		border: 1px solid #2a4d8f;
		border-radius: 5px;
	}
	.smb-section legend {
		padding: 0 0.5rem;
		color: #80a8e6;
		font-size: 0.88rem;
		font-weight: 500;
	}
	.smb-intro {
		margin: 0 0 0.3rem;
		color: #9aa3b2;
		font-size: 0.85rem;
	}
	.smb-row {
		display: flex;
		gap: 0.5rem;
	}
	.smb-row input {
		flex: 1;
	}

	input[type='text'],
	input[type='password'],
	select {
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
	input[type='text']:focus,
	input[type='password']:focus,
	select:focus {
		outline: none;
		border-color: #355fb0;
	}

	.mirror-note {
		margin: 0;
		padding: 0.7rem 0.9rem;
		background: #1f242c;
		border-left: 3px solid #80a8e6;
		border-radius: 4px;
		color: #c5cad3;
		font-size: 0.88rem;
		max-width: 640px;
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
