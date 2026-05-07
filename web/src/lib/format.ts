// Display helpers shared across pages. Pure presentation — no data
// logic here (sorting/filtering live in kovre-wasm per Phase 2 DoD).

export function formatBytes(n: number | null | undefined): string {
	if (n === null || n === undefined) return '—';
	if (n < 1024) return `${n} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let v = n / 1024;
	let unit = 0;
	while (v >= 1024 && unit < units.length - 1) {
		v /= 1024;
		unit++;
	}
	return `${v.toFixed(1)} ${units[unit]}`;
}

export function formatTime(iso: string | null | undefined): string {
	if (!iso) return '—';
	// `kovre_core::backup::snap_to_info` round-trips rustic's
	// `jiff::Zoned::Display`, which carries the RFC 9557 IANA-tz suffix
	// `[+02:00]`. Strip it so the JS Date parser is happy.
	const cleaned = iso.replace(/\[[^\]]+\]$/, '');
	const d = new Date(cleaned);
	if (isNaN(d.getTime())) return iso;
	return d.toLocaleString();
}

export function shortId(id: string | null | undefined, n = 8): string {
	if (!id) return '—';
	return id.length > n ? id.slice(0, n) : id;
}

/** Status → CSS class fragment used by the runs/jobs tables. */
export function statusClass(status: string): string {
	return `row-${status}`;
}
