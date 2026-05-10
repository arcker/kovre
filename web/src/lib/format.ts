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

/// Compact "2h ago" / "3d ago" rendering. Falls back to absolute
/// time for stamps older than a year, returns `—` for null.
export function formatRelative(iso: string | null | undefined): string {
	if (!iso) return '—';
	const cleaned = iso.replace(/\[[^\]]+\]$/, '');
	const d = new Date(cleaned);
	if (isNaN(d.getTime())) return iso;
	const diff = Date.now() - d.getTime();
	const sec = Math.floor(diff / 1000);
	if (sec < 60) return `${Math.max(sec, 0)}s ago`;
	const min = Math.floor(sec / 60);
	if (min < 60) return `${min}m ago`;
	const hr = Math.floor(min / 60);
	if (hr < 24) return `${hr}h ago`;
	const day = Math.floor(hr / 24);
	if (day < 30) return `${day}d ago`;
	const month = Math.floor(day / 30);
	if (month < 12) return `${month}mo ago`;
	return d.toLocaleDateString();
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
