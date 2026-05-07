import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';
import wasm from 'vite-plugin-wasm';

// `vite-plugin-wasm` lets us `import init, { sortRunsBy } from
// '../../kovre-wasm/pkg/kovre_wasm.js'` and have Vite fetch the .wasm
// alongside it without a separate bundling step. We deliberately keep
// `await init()` inside component lifecycle functions (e.g. `onMount`),
// not at module scope, so we don't need a top-level-await polyfill — a
// nicety on Vite 8 / Rolldown where the legacy
// `vite-plugin-top-level-await` is no longer compatible.
//
// In dev mode every `/api/*` request is proxied to the running
// `kovre serve` instance on :18080. The frontend therefore never has to
// know about CORS or absolute URLs — same-origin in production (where
// Lithair serves the bundled assets) and same-origin-by-proxy in dev.

export default defineConfig({
	plugins: [wasm(), sveltekit()],
	server: {
		// Bind to IPv4 explicitly. Vite 8 defaults to `localhost`, which
		// on dual-stack Windows resolves only to ::1 and is unreachable
		// from `curl 127.0.0.1:5173`. Forcing 127.0.0.1 keeps it
		// reachable through both IP families on this host.
		host: '127.0.0.1',
		port: 5173,
		proxy: {
			'/api': 'http://127.0.0.1:18080',
			'/health': 'http://127.0.0.1:18080',
			'/ready': 'http://127.0.0.1:18080',
			'/info': 'http://127.0.0.1:18080',
			'/_admin': 'http://127.0.0.1:18080'
		}
	}
});
