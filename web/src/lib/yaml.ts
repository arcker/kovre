// Tiny YAML emitter scoped to kovre's job schema.
//
// We don't pull in a full client-side YAML library: the only emission
// the dashboard does is appending a single job entry to an existing
// kovre.yaml whose shape is well known (Phase 1 schema, validated
// server-side on PUT). String concatenation is enough.
//
// Note: comments and unusual formatting in the existing YAML are
// preserved up to the appended block, but the appended block itself is
// canonicalized (2-space indent, no comments, list-of-strings as
// hyphenated entries). This matches what the server emits after any
// subsequent PUT.

export interface JobDraft {
	name: string;
	repository: string;
	template?: string | null;
	template_options?: Record<string, unknown>;
	paths?: string[];
	excludes?: string[];
	retention?: Record<string, number>;
}

/** Produce the YAML lines for a single job entry, indented for nesting
 *  under `jobs:` (2 spaces for the key, 4 for nested fields). */
function jobYaml(draft: JobDraft): string[] {
	const out: string[] = [`  ${draft.name}:`];
	if (draft.template) out.push(`    template: ${draft.template}`);
	out.push(`    repository: ${draft.repository}`);

	if (draft.template_options && Object.keys(draft.template_options).length > 0) {
		out.push(`    template_options:`);
		for (const [k, v] of Object.entries(draft.template_options)) {
			if (v == null || v === '') continue;
			out.push(`      ${k}: ${formatScalar(v)}`);
		}
	}

	if (draft.paths && draft.paths.length > 0) {
		out.push(`    paths:`);
		for (const p of draft.paths) out.push(`      - ${formatScalar(p)}`);
	}

	if (draft.excludes && draft.excludes.length > 0) {
		out.push(`    excludes:`);
		for (const e of draft.excludes) out.push(`      - ${formatScalar(e)}`);
	}

	if (draft.retention && Object.keys(draft.retention).length > 0) {
		out.push(`    retention:`);
		for (const [k, v] of Object.entries(draft.retention)) {
			if (v == null) continue;
			out.push(`      ${k}: ${v}`);
		}
	}

	return out;
}

/** Quote a YAML scalar when needed — paths with Windows backslashes,
 *  glob patterns with `*`, anything starting with a special char.
 *  Bare alphanumerics stay unquoted. */
function formatScalar(v: unknown): string {
	const s = String(v);
	if (s === '') return '""';
	const needsQuote =
		/[\\:#?@`&*!|>%'"\[\]{},]/.test(s) ||
		s.startsWith(' ') ||
		s.endsWith(' ') ||
		/^[-?]/.test(s);
	if (!needsQuote) return s;
	// Use double quotes; escape backslashes and double quotes.
	return `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

/** Append a new job entry to the current YAML text. The existing
 *  content is preserved verbatim; the new block is canonical. */
export function appendJobToYaml(currentYaml: string, draft: JobDraft): string {
	const block = jobYaml(draft).join('\n');
	const base = currentYaml.replace(/\s+$/, '');
	const hasJobsKey = /^jobs\s*:\s*$/m.test(base) || /^jobs\s*:/m.test(base);

	if (!hasJobsKey) {
		return `${base}\n\njobs:\n${block}\n`;
	}
	return `${base}\n${block}\n`;
}
