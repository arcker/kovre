// Typed fetch helpers for kovre's `/api/*` endpoints.
//
// Every helper returns the parsed `data` array directly: Lithair's
// auto-CRUD wraps every list response in `{data, total, skip, take,
// has_more}`, but the dashboard mostly cares about the `data` rows.
// When we need pagination metadata (step 9+), we'll switch to
// returning the full envelope.

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

export const listJobRuns = (): Promise<JobRun[]> => getList<JobRun>('/api/job_runs');
export const listSnapshots = (): Promise<Snapshot[]> => getList<Snapshot>('/api/snapshots');
