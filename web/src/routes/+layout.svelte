<script lang="ts">
	let { children } = $props();
</script>

<header>
	<h1>kovre</h1>
	<nav>
		<a href="/">runs</a>
	</nav>
</header>

<main>
	{@render children()}
</main>

<style>
	:global(body) {
		font-family: ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif;
		margin: 0;
		padding: 0;
		background: #0f1115;
		color: #e6e8eb;
	}

	header {
		display: flex;
		align-items: baseline;
		gap: 2rem;
		padding: 1rem 2rem;
		border-bottom: 1px solid #2a2f38;
	}

	header h1 {
		margin: 0;
		font-size: 1.25rem;
		font-weight: 600;
		letter-spacing: 0.02em;
	}

	header nav a {
		color: #9aa3b2;
		text-decoration: none;
		font-size: 0.95rem;
	}

	header nav a:hover {
		color: #e6e8eb;
	}

	main {
		padding: 2rem;
		max-width: 1200px;
		margin: 0 auto;
	}
</style>
