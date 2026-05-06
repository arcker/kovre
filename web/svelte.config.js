import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),

	kit: {
		// SPA mode: every route falls back to index.html. The dashboard
		// is purely client-rendered (auth state, live runs, WASM-driven
		// sort/filter), so SSR / prerender would just add complexity
		// for no benefit.
		adapter: adapter({
			fallback: 'index.html',
			pages: 'build',
			assets: 'build',
			strict: false
		})
	}
};

export default config;
