// Pure SPA mode. Disables SvelteKit's server-side rendering and the
// build-time prerender pass: the dashboard depends on a running kovre
// backend (it reads /api/* live), so prerender would either fail or
// produce stale HTML.
export const ssr = false;
export const prerender = false;
