<script lang="ts" module>
	// Inline SVG icon set used across the app. Paths are lifted verbatim from the
	// design comps so stroke weights / shapes match exactly.
	export type IconName =
		| 'search'
		| 'heart'
		| 'help'
		| 'star'
		| 'plus'
		| 'check'
		| 'share'
		| 'play'
		| 'chevron-right'
		| 'chevron-left'
		| 'chevron-down'
		| 'sliders'
		| 'sliders-h'
		| 'list'
		| 'arrow-right'
		| 'comment'
		| 'reply'
		| 'alert'
		| 'gear'
		| 'x'
		| 'card'
		| 'mail'
		| 'bookmark'
		| 'flame';

	const FILLED = new Set<IconName>(['star', 'play', 'flame']);
</script>

<script lang="ts">
	let {
		name,
		size = 20,
		fill = 'none',
		stroke = 'currentColor',
		strokeWidth = 2,
	}: {
		name: IconName;
		size?: number;
		fill?: string;
		stroke?: string;
		strokeWidth?: number;
	} = $props();

	const isFilled = $derived(FILLED.has(name));
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill={isFilled ? (fill === 'none' ? 'currentColor' : fill) : fill}
	stroke={isFilled ? 'none' : stroke}
	stroke-width={strokeWidth}
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"
>
	{#if name === 'search'}
		<circle cx="11" cy="11" r="7" /><path d="M21 21l-4.35-4.35" />
	{:else if name === 'heart'}
		<path
			d="M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.6l-1-1a5.5 5.5 0 1 0-7.8 7.8L12 21l8.8-8.6a5.5 5.5 0 0 0 0-7.8z"
		/>
	{:else if name === 'help'}
		<circle cx="12" cy="12" r="9" /><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 2.5-3 4" /><line
			x1="12"
			y1="17"
			x2="12.01"
			y2="17"
		/>
	{:else if name === 'star'}
		<path d="M12 2l2.9 6.3 6.9.7-5.1 4.6 1.4 6.8L12 17.6 5.9 20.4l1.4-6.8L2.2 9l6.9-.7z" />
	{:else if name === 'plus'}
		<path d="M12 5v14M5 12h14" />
	{:else if name === 'check'}
		<path d="M20 6L9 17l-5-5" />
	{:else if name === 'share'}
		<circle cx="18" cy="5" r="3" /><circle cx="6" cy="12" r="3" /><circle
			cx="18"
			cy="19"
			r="3"
		/><path d="M8.6 13.5l6.8 4M15.4 6.5l-6.8 4" />
	{:else if name === 'play'}
		<path d="M6 4l14 8-14 8z" />
	{:else if name === 'chevron-right'}
		<path d="M9 6l6 6-6 6" />
	{:else if name === 'chevron-left'}
		<path d="M15 18l-6-6 6-6" />
	{:else if name === 'chevron-down'}
		<path d="M6 9l6 6 6-6" />
	{:else if name === 'sliders'}
		<path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6" />
	{:else if name === 'sliders-h'}
		<path d="M4 6h8" /><path d="M16 6h4" /><circle cx="14" cy="6" r="2" /><path d="M4 12h4" /><path
			d="M12 12h8"
		/><circle cx="10" cy="12" r="2" /><path d="M4 18h8" /><path d="M16 18h4" /><circle
			cx="14"
			cy="18"
			r="2"
		/>
	{:else if name === 'list'}
		<line x1="8" y1="6" x2="21" y2="6" /><line x1="8" y1="12" x2="21" y2="12" /><line
			x1="8"
			y1="18"
			x2="21"
			y2="18"
		/><line x1="3" y1="6" x2="3.01" y2="6" /><line x1="3" y1="12" x2="3.01" y2="12" /><line
			x1="3"
			y1="18"
			x2="3.01"
			y2="18"
		/>
	{:else if name === 'arrow-right'}
		<path d="M5 12h14M13 6l6 6-6 6" />
	{:else if name === 'comment'}
		<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
	{:else if name === 'reply'}
		<polyline points="9 17 4 12 9 7" /><path d="M20 18v-2a4 4 0 0 0-4-4H4" />
	{:else if name === 'alert'}
		<path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" /><line
			x1="12"
			y1="9"
			x2="12"
			y2="13"
		/><line x1="12" y1="17" x2="12.01" y2="17" />
	{:else if name === 'gear'}
		<circle cx="12" cy="12" r="3" /><path
			d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"
		/>
	{:else if name === 'x'}
		<line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
	{:else if name === 'card'}
		<rect x="3" y="5" width="18" height="14" rx="2" /><line x1="3" y1="10" x2="21" y2="10" />
	{:else if name === 'mail'}
		<rect x="3" y="5" width="18" height="14" rx="2" /><path d="M3 7l9 6 9-6" />
	{:else if name === 'bookmark'}
		<path d="M17 2H7a2 2 0 0 0-2 2v18l7-4 7 4V4a2 2 0 0 0-2-2z" />
	{:else if name === 'flame'}
		<path
			d="M12.5 2c.3 2.4-.7 4-1.9 5.4C9.3 8.9 8 10.3 8 12.6c0 .9.3 1.7.8 2.3-1.8-.4-3.1-1.9-3.4-3.8-.1-.5-.7-.7-1-.3C3.5 12 3 13.6 3 15.2 3 19 6.1 22 10 22c4.4 0 8-3.4 8-7.8 0-3.2-1.7-5.4-3.3-7.1C13.2 5.6 12 4.2 12.5 2z"
		/>
	{/if}
</svg>
