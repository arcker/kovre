// Typed fetch helpers for kovre's `/api/*` endpoints.

import type { ParsedConfig } from './yaml';

export interface JobRun {
	id: string;
	job_name: string;
	started_at: string;
	finished_at: string | null;
	status: 'running' | 'success' | 'failed' | string;
	failure_reason: string | null;
	snapshot_id: string | null;
	bytes_processed: number | null;
	bytes_added: number | null;
	trigger: 'cli' | 'dashboard' | 'scheduled' | string;
}

export interface Snapshot {
	id: string;
	job_name: string;
	time: string;
	paths: string[];
	hostname: string;
	bytes_total: number | null;
}

export interface Retention {
	keep_last?: number | null;
	keep_hourly?: number | null;
	keep_daily?: number | null;
	keep_weekly?: number | null;
	keep_monthly?: number | null;
	keep_yearly?: number | null;
	/** Mirror backend only: how many archived versions to keep per file. */
	keep_versions?: number | null;
}

export interface Job {
	name: string;
	repository: string;
	template?: string | null;
	template_options?: unknown;
	paths?: string[] | null;
	excludes?: string[] | null;
	retention?: Retention | null;
}

interface ListEnvelope<T> {
	data: T[];
	total: number;
	skip: number;
	take: number;
	has_more: boolean;
}

async function getList<T>(path: string): Promise<T[]> {
	const resp = await fetch(path);
	if (!resp.ok) {
		throw new Error(`${path} → HTTP ${resp.status}`);
	}
	const body: ListEnvelope<T> = await resp.json();
	return body.data;
}

async function getJson<T>(path: string): Promise<T> {
	const resp = await fetch(path);
	if (!resp.ok) {
		throw new Error(`${path} → HTTP ${resp.status}`);
	}
	return resp.json();
}

export const listJobRuns = (): Promise<JobRun[]> => getList<JobRun>('/api/job_runs');
export const listSnapshots = (): Promise<Snapshot[]> => getList<Snapshot>('/api/snapshots');
export const listJobs = (): Promise<Job[]> => getJson<Job[]>('/api/jobs');

// --- Phase 3: templates + config edition --------------------------------

export interface TemplateOption {
	key: string;
	type: 'directory' | 'directory_list' | 'string_list' | 'bool';
	label: string;
	required: boolean;
	/** For typed options: the default the form should pre-fill (e.g.
	 *  `true` for a bool option). Untyped/missing means no default. */
	default?: unknown;
}

export interface Template {
	name: string;
	icon: string;
	description: string;
	options: TemplateOption[];
}

export interface FsEntry {
	name: string;
	is_dir: boolean;
}

export interface FsListing {
	path: string;
	entries: FsEntry[];
}

export interface ConfigPayload {
	yaml: string;
	parsed: ParsedConfig;
}

export const listTemplates = (): Promise<Template[]> => getJson<Template[]>('/api/templates');

export const listFs = (path: string): Promise<FsListing> =>
	getJson<FsListing>(`/api/fs?path=${encodeURIComponent(path)}`);

export interface FsStat {
	exists: boolean;
	is_file: boolean;
	is_dir: boolean;
	size?: number;
	path: string;
}

export const fsStat = (path: string): Promise<FsStat> =>
	getJson<FsStat>(`/api/fs/stat?path=${encodeURIComponent(path)}`);

/** Generate a fresh random passphrase server-side and write it to
 *  `path`. The passphrase itself never leaves the box — the response
 *  only confirms the length and target path. */
export async function initRepositoryPassword(path: string): Promise<{ path: string; length: number }> {
	const resp = await fetch('/api/repositories/init-password', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ path })
	});
	const body = await resp.json().catch(() => ({}) as Record<string, unknown>);
	if (resp.ok) {
		return body as { path: string; length: number };
	}
	const hint = typeof body.hint === 'string' ? ` (${body.hint})` : '';
	const reason = typeof body.reason === 'string' ? `: ${body.reason}` : '';
	throw new Error(`${body.error ?? `HTTP ${resp.status}`}${hint}${reason}`);
}

export interface RepositoryStatus {
	initialized: boolean;
}

/** Per-repo init state, keyed by repository name. Used by the
 *  /repositories list to hide the "init" button on repos that
 *  already have a rustic config file on disk. */
export const getRepositoriesStatus = (): Promise<Record<string, RepositoryStatus>> =>
	getJson<Record<string, RepositoryStatus>>('/api/repositories/status');

/** Initialize the rustic repository on disk (creates the `config`,
 *  `keys`, `data`, `index`, `snapshots` directories). Returns:
 *    - { ok: true, justInitialized: true }   when the repo was created
 *    - { ok: true, justInitialized: false }  when it was already initialized (409 → no-op)
 *    - throws Error                          on any other failure
 *  The caller can chain this after `putConfig` without branching on
 *  the "already exists" case, which is the common path when editing
 *  an existing repo. */
export async function initRepository(
	name: string
): Promise<{ ok: true; justInitialized: boolean }> {
	const resp = await fetch(
		`/api/repositories/${encodeURIComponent(name)}/init`,
		{ method: 'POST' }
	);
	const body = await resp.json().catch(() => ({}) as Record<string, unknown>);
	if (resp.status === 200) {
		return { ok: true, justInitialized: true };
	}
	if (resp.status === 409 && body.error === 'already_initialized') {
		return { ok: true, justInitialized: false };
	}
	const message =
		typeof body.message === 'string'
			? body.message
			: typeof body.error === 'string'
				? body.error
				: `HTTP ${resp.status}`;
	throw new Error(message);
}

export const getConfig = (): Promise<ConfigPayload> => getJson<ConfigPayload>('/api/config');

export interface ResolvedTemplate {
	name: string;
	paths: string[];
	excludes: string[];
	status: 'ok' | 'empty' | string;
}

/** Ask the server to expand a template into concrete paths/excludes
 *  on this machine. Used by the inventory view to show "what's
 *  actually being backed up" per job. Treats 400 (`custom` is not a
 *  template) as a soft failure rather than throwing. */
export async function resolveTemplate(
	name: string,
	options: Record<string, unknown> | null = null
): Promise<ResolvedTemplate | null> {
	const resp = await fetch(`/api/templates/${encodeURIComponent(name)}/resolve`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(options ?? {})
	});
	if (resp.status === 400 || resp.status === 404) {
		return null; // `custom` pseudo-template or unknown name
	}
	if (!resp.ok) {
		throw new Error(`POST /api/templates/${name}/resolve → HTTP ${resp.status}`);
	}
	return resp.json();
}

export interface VerifyOutcome {
	ok: boolean;
	messages: string[];
	name: string;
}

/** Run an integrity check on a repository. Always resolves (server
 *  returns 200 even when ok=false); throws only on transport / 4xx. */
export async function verifyRepository(name: string): Promise<VerifyOutcome> {
	const resp = await fetch(`/api/repositories/${encodeURIComponent(name)}/verify`, {
		method: 'POST'
	});
	const body = await resp.json().catch(() => ({}) as Record<string, unknown>);
	if (resp.ok) {
		return body as VerifyOutcome;
	}
	const message =
		typeof body.message === 'string'
			? body.message
			: typeof body.error === 'string'
				? body.error
				: `HTTP ${resp.status}`;
	throw new Error(message);
}

/** Replace the running config. Server validates the YAML before
 *  touching the file or the in-memory state; on failure the response
 *  body carries the parse error and the optional line/column. */
export async function putConfig(yaml: string): Promise<ConfigPayload> {
	const resp = await fetch('/api/config', {
		method: 'PUT',
		headers: { 'Content-Type': 'application/yaml' },
		body: yaml
	});
	const body = await resp.json().catch(() => ({}) as Record<string, unknown>);
	if (resp.status === 200) {
		return body as ConfigPayload;
	}
	const message = typeof body.message === 'string' ? body.message : `HTTP ${resp.status}`;
	const location = body.location as { line?: number; column?: number } | undefined;
	const where = location?.line != null ? ` (line ${location.line}, col ${location.column ?? '?'})` : '';
	throw new Error(`${message}${where}`);
}

/** Trigger a backup. Resolves to the new run id, or throws with a
 *  reason that includes the existing run id when 409 Conflict. */
export async function triggerRun(jobName: string): Promise<string> {
	const resp = await fetch(`/api/jobs/${encodeURIComponent(jobName)}/run`, {
		method: 'POST'
	});
	const body = await resp.json().catch(() => ({}));
	if (resp.status === 202) {
		return body.id;
	}
	if (resp.status === 409) {
		throw new Error(`already running (run_id=${body.run_id ?? '?'})`);
	}
	if (resp.status === 404) {
		throw new Error(`unknown job: ${jobName}`);
	}
	throw new Error(`POST /api/jobs/${jobName}/run → HTTP ${resp.status}`);
}
