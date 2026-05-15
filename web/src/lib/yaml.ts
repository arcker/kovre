// Minimal YAML emitter scoped to kovre's config schema.
//
// The previous Phase 3 version (`appendJobToYaml`) preserved the
// existing YAML text and only canonicalized the appended block. With
// edit/delete operations landing in Phase 3.5, that surface-mutation
// approach breaks down — we now own the full file. Every PUT
// rebuilds the YAML from the parsed structure, so comments are lost
// on the first edit through the UI (documented in README).

export interface ParsedConfig {
	agent: { data_dir: string; log_level: string };
	repositories: Record<string, RepositoryEntry>;
	jobs: Record<string, JobEntry>;
}

export type BackendKind = 'rustic' | 'mirror';

export interface RepositoryEntry {
	path: string;
	/** Storage format. Defaults to 'rustic' when omitted in the YAML. */
	backend?: BackendKind;
	/** Required for rustic, optional for mirror (mirror has no passphrase). */
	password_file?: string;
}

export interface JobEntry {
	repository: string;
	template?: string | null;
	template_options?: Record<string, unknown>;
	paths?: string[] | null;
	excludes?: string[] | null;
	retention?: Record<string, number | null | undefined> | null;
}

export interface JobDraft {
	name: string;
	repository: string;
	template?: string | null;
	template_options?: Record<string, unknown>;
	paths?: string[];
	excludes?: string[];
	retention?: Record<string, number>;
}

export interface RepositoryDraft {
	name: string;
	path: string;
	backend: BackendKind;
	/** Empty string when the form doesn't need it (mirror). */
	password_file: string;
}

/** Quote a YAML scalar when needed — Windows paths with backslashes,
 *  glob patterns, anything starting with a special character. Bare
 *  alphanumerics, dots and dashes stay unquoted. */
function scalar(v: unknown): string {
	const s = String(v);
	if (s === '') return '""';
	const needsQuote =
		/[\\:#?@`&*!|>%'"\[\]{},]/.test(s) ||
		s.startsWith(' ') ||
		s.endsWith(' ') ||
		/^[-?]/.test(s);
	if (!needsQuote) return s;
	return `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

function emitAgent(agent: ParsedConfig['agent']): string {
	return [
		`agent:`,
		`  data_dir: ${scalar(agent.data_dir)}`,
		`  log_level: ${scalar(agent.log_level)}`
	].join('\n');
}

function emitRepositories(repos: Record<string, RepositoryEntry>): string {
	if (Object.keys(repos).length === 0) return 'repositories: {}';
	const out: string[] = ['repositories:'];
	for (const [name, entry] of Object.entries(repos)) {
		out.push(`  ${scalar(name)}:`);
		out.push(`    path: ${scalar(entry.path)}`);
		// Only emit `backend:` when it diverges from the rustic default,
		// to keep existing kovre.yaml files visually unchanged when the
		// dashboard rewrites them.
		if (entry.backend && entry.backend !== 'rustic') {
			out.push(`    backend: ${scalar(entry.backend)}`);
		}
		if (entry.password_file && entry.password_file.length > 0) {
			out.push(`    password_file: ${scalar(entry.password_file)}`);
		}
	}
	return out.join('\n');
}

/** Same shape as `serve::runs::JobRun`'s YAML serialization but
 *  pretty-printed. Used by both the template wizard and the
 *  edit-job/delete-job flows. */
function emitJob(name: string, job: JobEntry | JobDraft): string {
	const lines: string[] = [`  ${scalar(name)}:`];

	if (job.template) lines.push(`    template: ${scalar(job.template)}`);
	lines.push(`    repository: ${scalar(job.repository)}`);

	if (job.template_options && Object.keys(job.template_options).length > 0) {
		lines.push(`    template_options:`);
		for (const [k, v] of Object.entries(job.template_options)) {
			if (v == null || v === '') continue;
			lines.push(`      ${k}: ${scalar(v)}`);
		}
	}

	if (job.paths && job.paths.length > 0) {
		lines.push(`    paths:`);
		for (const p of job.paths) lines.push(`      - ${scalar(p)}`);
	}

	if (job.excludes && job.excludes.length > 0) {
		lines.push(`    excludes:`);
		for (const e of job.excludes) lines.push(`      - ${scalar(e)}`);
	}

	if (job.retention && Object.keys(job.retention).length > 0) {
		const used = Object.entries(job.retention).filter(([, v]) => v != null);
		if (used.length > 0) {
			lines.push(`    retention:`);
			for (const [k, v] of used) lines.push(`      ${k}: ${v}`);
		}
	}

	return lines.join('\n');
}

function emitJobs(jobs: Record<string, JobEntry>): string {
	if (Object.keys(jobs).length === 0) return 'jobs: {}';
	const out: string[] = ['jobs:'];
	for (const [name, job] of Object.entries(jobs)) {
		out.push(emitJob(name, job));
	}
	return out.join('\n');
}

/** Re-emit the full kovre.yaml from a parsed structure. Canonical
 *  ordering: `agent` → `repositories` → `jobs`. Within each section
 *  entries appear in insertion order (the caller controls that). */
export function emitConfigYaml(parsed: ParsedConfig): string {
	return [emitAgent(parsed.agent), '', emitRepositories(parsed.repositories), '', emitJobs(parsed.jobs)].join('\n') + '\n';
}

// ---- Mutation helpers ------------------------------------------------
//
// Each returns a fresh ParsedConfig — never mutates the input. Plug
// the result into emitConfigYaml + PUT /api/config.

export function addJob(parsed: ParsedConfig, draft: JobDraft): ParsedConfig {
	const next: ParsedConfig = {
		agent: parsed.agent,
		repositories: { ...parsed.repositories },
		jobs: { ...parsed.jobs, [draft.name]: draftToEntry(draft) }
	};
	return next;
}

export function updateJob(parsed: ParsedConfig, name: string, draft: JobDraft): ParsedConfig {
	const jobs = { ...parsed.jobs };
	// Preserve the existing insertion order: rebuild the map, replacing
	// the entry in place. Object spread alone would push renamed keys to
	// the end, which would visibly re-shuffle the YAML.
	const ordered: Record<string, JobEntry> = {};
	for (const k of Object.keys(parsed.jobs)) {
		if (k === name) {
			ordered[draft.name] = draftToEntry(draft);
		} else {
			ordered[k] = jobs[k];
		}
	}
	// If the rename moved an entry away from where the old key sat,
	// the new key may also need to land. Cover the case where the new
	// name didn't already exist anywhere.
	if (!Object.prototype.hasOwnProperty.call(ordered, draft.name)) {
		ordered[draft.name] = draftToEntry(draft);
	}
	return { agent: parsed.agent, repositories: { ...parsed.repositories }, jobs: ordered };
}

export function removeJob(parsed: ParsedConfig, name: string): ParsedConfig {
	const jobs = { ...parsed.jobs };
	delete jobs[name];
	return { agent: parsed.agent, repositories: { ...parsed.repositories }, jobs };
}

function draftToRepoEntry(draft: RepositoryDraft): RepositoryEntry {
	const entry: RepositoryEntry = { path: draft.path, backend: draft.backend };
	const pwd = draft.password_file.trim();
	if (pwd.length > 0) entry.password_file = pwd;
	return entry;
}

export function addRepository(parsed: ParsedConfig, draft: RepositoryDraft): ParsedConfig {
	return {
		agent: parsed.agent,
		repositories: {
			...parsed.repositories,
			[draft.name]: draftToRepoEntry(draft)
		},
		jobs: { ...parsed.jobs }
	};
}

export function updateRepository(
	parsed: ParsedConfig,
	name: string,
	draft: RepositoryDraft
): ParsedConfig {
	const ordered: Record<string, RepositoryEntry> = {};
	for (const k of Object.keys(parsed.repositories)) {
		if (k === name) {
			ordered[draft.name] = draftToRepoEntry(draft);
		} else {
			ordered[k] = parsed.repositories[k];
		}
	}
	if (!Object.prototype.hasOwnProperty.call(ordered, draft.name)) {
		ordered[draft.name] = draftToRepoEntry(draft);
	}

	// Repository renames also have to update any job that referenced the
	// old name — otherwise the PUT will be rejected as
	// `UnknownRepository`. We rewrite jobs to point at the new name.
	const jobs: Record<string, JobEntry> = {};
	for (const [k, job] of Object.entries(parsed.jobs)) {
		jobs[k] = {
			...job,
			repository: job.repository === name ? draft.name : job.repository
		};
	}
	return { agent: parsed.agent, repositories: ordered, jobs };
}

export function removeRepository(parsed: ParsedConfig, name: string): ParsedConfig {
	const repositories = { ...parsed.repositories };
	delete repositories[name];
	return { agent: parsed.agent, repositories, jobs: { ...parsed.jobs } };
}

/** Jobs that currently reference a given repository — used to refuse
 *  a delete that would leave dangling references. */
export function jobsUsingRepository(parsed: ParsedConfig, repo: string): string[] {
	return Object.entries(parsed.jobs)
		.filter(([, job]) => job.repository === repo)
		.map(([name]) => name);
}

function draftToEntry(draft: JobDraft): JobEntry {
	const out: JobEntry = { repository: draft.repository };
	if (draft.template) out.template = draft.template;
	if (draft.template_options && Object.keys(draft.template_options).length > 0) {
		out.template_options = draft.template_options;
	}
	if (draft.paths && draft.paths.length > 0) out.paths = draft.paths;
	if (draft.excludes && draft.excludes.length > 0) out.excludes = draft.excludes;
	if (draft.retention && Object.keys(draft.retention).length > 0) {
		out.retention = draft.retention as Record<string, number>;
	}
	return out;
}

