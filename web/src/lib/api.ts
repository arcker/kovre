// Typed fetch helpers for kovre's `/api/*` endpoints.

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
