<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import { slug } from '$lib/data/mock';
	import { getProfile, updateProfile, uploadAvatar, type ProfileView } from '$lib/data/source';
	import { auth } from '$lib/auth.svelte';
	import { backend } from '$lib/context';

	let { data } = $props();

	// NSFW visibility preference (CATALOGUE.md §2). Reflects the signed-in user's
	// server-side setting; the toggle persists via `setShowNsfw` and updates auth.
	const showNsfw = $derived(auth.user?.showNsfw ?? false);
	let savingNsfw = $state(false);
	let nsfwError = $state('');
	async function toggleNsfw(): Promise<void> {
		if (!auth.user || savingNsfw || !backend.setShowNsfw) return;
		savingNsfw = true;
		nsfwError = '';
		try {
			const next = await backend.setShowNsfw(!auth.user.showNsfw);
			if (auth.user) auth.user.showNsfw = next;
		} catch (err) {
			nsfwError = err instanceof Error ? err.message : 'Could not update the setting.';
		} finally {
			savingNsfw = false;
		}
	}
	// Re-fetch once auth has restored the session token onto the backend — the
	// initial `load` can race ahead of `initAuth`, so without this the signed-in
	// user's real profile wouldn't resolve on first paint. Falls back to the load
	// result (mock when signed out / backend off) until then.
	let liveProfile = $state<ProfileView | null>(null);
	const profile = $derived(liveProfile ?? data.profile);
	function refreshProfile(): void {
		getProfile().then((p) => {
			liveProfile = p;
		});
	}
	$effect(() => {
		void auth.ready;
		void auth.user?.id;
		if (!auth.ready) return;
		refreshProfile();
	});

	// --- Edit profile (display name + bio) + avatar upload ------------------
	let editing = $state(false);
	let editName = $state('');
	let editBio = $state('');
	let savingProfile = $state(false);
	let profileError = $state('');
	let uploadingAvatar = $state(false);
	let avatarError = $state('');
	let fileInput = $state<HTMLInputElement | null>(null);

	const canEdit = $derived(!!auth.user && !!backend.updateProfile);

	function openEditor(): void {
		if (!auth.user) return;
		editName = auth.user.displayName ?? '';
		editBio = auth.user.bio ?? '';
		profileError = '';
		editing = true;
	}

	async function saveProfile(): Promise<void> {
		if (savingProfile) return;
		savingProfile = true;
		profileError = '';
		try {
			const updated = await updateProfile({ displayName: editName, bio: editBio });
			if (updated && auth.user) {
				auth.user.displayName = updated.displayName;
				auth.user.bio = updated.bio;
			}
			editing = false;
			refreshProfile();
		} catch (err) {
			profileError = err instanceof Error ? err.message : 'Could not save your profile.';
		} finally {
			savingProfile = false;
		}
	}

	async function onAvatarPicked(e: Event): Promise<void> {
		const input = e.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		uploadingAvatar = true;
		avatarError = '';
		try {
			const url = await uploadAvatar(file);
			if (auth.user) auth.user.avatarUrl = url;
			refreshProfile();
		} catch (err) {
			avatarError = err instanceof Error ? err.message : 'Could not upload the image.';
		} finally {
			uploadingAvatar = false;
			input.value = ''; // allow re-picking the same file
		}
	}

	let tab = $state<'reading' | 'completed' | 'favorites'>('reading');

	const counts = $derived({
		reading: profile.shelves.filter((c) => c.shelf === 'reading').length,
		completed: profile.shelves.filter((c) => c.shelf === 'completed').length,
		favorites: profile.shelves.filter((c) => c.shelf === 'favorites').length,
	});

	const tabDefs = [
		{ key: 'reading', label: 'Reading' },
		{ key: 'completed', label: 'Completed' },
		{ key: 'favorites', label: 'Favorites' },
	] as const;

	const shelfItems = $derived(
		profile.shelves
			.filter((c) => c.shelf === tab)
			.map((c) => ({
				...c,
				sub: c.shelf === 'reading' ? `Ch. ${c.ch} / ${c.total}` : `${c.genre} · ${c.total} ch`,
			})),
	);
</script>

<div class="head k-gutter">
	<div class="identity">
		<div class="avatar-wrap">
			<Avatar
				url={profile.avatarUrl}
				name={profile.name}
				colorKey={profile.id || profile.name}
				size={104}
			/>
			{#if canEdit}
				<button
					class="avatar-edit"
					aria-label="Change profile photo"
					title="Change profile photo"
					disabled={uploadingAvatar}
					onclick={() => fileInput?.click()}
				>
					{#if uploadingAvatar}<span class="spin"></span>{:else}<span class="cam" aria-hidden="true">📷</span>{/if}
				</button>
				<input
					bind:this={fileInput}
					type="file"
					accept="image/png,image/jpeg,image/webp"
					class="visually-hidden"
					onchange={onAvatarPicked}
				/>
			{/if}
		</div>
		<div class="who">
			<div class="name-row">
				<h1>{profile.name}</h1>
				<span class="pro">{profile.badge}</span>
			</div>
			<div class="handle">{profile.handle} · {profile.since}</div>
			<p class="bio">{profile.bio}</p>
			{#if avatarError}<p class="setting-error">{avatarError}</p>{/if}
		</div>
		{#if canEdit}
			<div class="actions">
				<button class="edit" onclick={openEditor}>Edit profile</button>
				<button class="gear" aria-label="Settings"><Icon name="gear" size={18} /></button>
			</div>
		{/if}
	</div>

	{#if editing}
		<div class="editor">
			<label class="field">
				<span>Display name</span>
				<input type="text" bind:value={editName} maxlength="50" placeholder={auth.user?.username} />
			</label>
			<label class="field">
				<span>Bio</span>
				<textarea bind:value={editBio} maxlength="500" rows="3" placeholder="Tell readers about yourself…"
				></textarea>
			</label>
			{#if profileError}<p class="setting-error">{profileError}</p>{/if}
			<div class="editor-actions">
				<button class="cancel" onclick={() => (editing = false)} disabled={savingProfile}>Cancel</button>
				<button class="save" onclick={saveProfile} disabled={savingProfile}>
					{savingProfile ? 'Saving…' : 'Save changes'}
				</button>
			</div>
		</div>
	{/if}

	<div class="stats">
		{#each profile.stats as s (s.label)}
			<div class="stat">
				<span class="stat-value">{s.value}</span>
				<span class="stat-label">{s.label}</span>
			</div>
		{/each}
	</div>
</div>

<div class="body k-gutter">
	<div class="left">
		<div class="section">
			<h2 class="section-label">Currently Reading</h2>
			<div class="reading-list">
				{#each profile.reading as r (r.title)}
					{@const pct = Math.round((r.ch / r.total) * 100)}
					<a class="reading-row" href={`/series/${r.id ?? slug(r.title)}`}>
						<div class="mini-cover k-cover"></div>
						<div class="reading-info">
							<div class="reading-top">
								<span class="reading-title">{r.title}</span>
								<span class="reading-ch">{r.ch} / {r.total}</span>
							</div>
							<div class="reading-genre">{r.genre}</div>
							<div class="reading-bar"><div class="fill" style="width:{pct}%"></div></div>
						</div>
					</a>
				{/each}
			</div>
		</div>

		<div class="section">
			<div class="tabs">
				{#each tabDefs as t (t.key)}
					<button class="tab" class:on={tab === t.key} onclick={() => (tab = t.key)}>
						{t.label}<span class="count">{counts[t.key]}</span>
					</button>
				{/each}
			</div>
			<div class="shelf-grid">
				{#each shelfItems as item (item.title + item.shelf)}
					<a class="shelf-card" href={`/series/${item.id ?? slug(item.title)}`}>
						<div class="cover k-cover">
							<span class="rating"
								><Icon name="star" size={9} fill="var(--k-star)" />{item.rating}</span
							>
						</div>
						<div class="shelf-title">{item.title}</div>
						<div class="shelf-sub">{item.sub}</div>
					</a>
				{/each}
			</div>
		</div>
	</div>

	<div class="right">
		{#if auth.user}
			<div class="card">
				<h3 class="card-title">Content Settings</h3>
				<div class="setting-row">
					<div class="setting-text">
						<span class="setting-label">Show NSFW content</span>
						<span class="setting-desc">
							Include adult-rated series in browse, search, and updates.
						</span>
					</div>
					<button
						type="button"
						class="switch"
						class:on={showNsfw}
						role="switch"
						aria-checked={showNsfw}
						aria-label="Show NSFW content"
						disabled={savingNsfw}
						onclick={toggleNsfw}
					>
						<span class="knob"></span>
					</button>
				</div>
				{#if nsfwError}<p class="setting-error">{nsfwError}</p>{/if}
			</div>
		{/if}

		<div class="card">
			<h3 class="card-title">Favorite Genres</h3>
			<div class="genres">
				{#each profile.favGenres as g (g.name)}
					<div class="genre">
						<div class="genre-top">
							<span class="gname">{g.name}</span><span class="gpct">{g.pct}%</span>
						</div>
						<div class="genre-bar"><div class="fill" style="width:{g.pct}%"></div></div>
					</div>
				{/each}
			</div>
		</div>

		<div class="section">
			<h3 class="section-label">Recent Activity</h3>
			<div class="activity">
				{#each profile.activity as a (a.text)}
					<div class="act-row">
						<div class="act-icon" style="background:{a.iconBg}">{a.icon}</div>
						<div class="act-body">
							<div class="act-text">{a.text}</div>
							<div class="act-time">{a.time}</div>
						</div>
					</div>
				{/each}
			</div>
		</div>
	</div>
</div>

<Footer />

<style>
	.head {
		padding-top: 52px;
	}
	.identity {
		display: flex;
		gap: 28px;
		align-items: flex-start;
		flex-wrap: wrap;
	}
	.avatar-wrap {
		flex: 0 0 auto;
		position: relative;
		width: 104px;
		height: 104px;
	}
	.avatar-edit {
		position: absolute;
		right: -2px;
		bottom: -2px;
		width: 34px;
		height: 34px;
		border-radius: 50%;
		background: var(--k-primary);
		color: var(--k-on-primary);
		border: 2px solid var(--k-bg, #0c0c0d);
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		font-size: 15px;
		line-height: 1;
		transition: filter 0.15s;
	}
	.avatar-edit:hover:not(:disabled) {
		filter: brightness(1.1);
	}
	.avatar-edit:disabled {
		opacity: 0.7;
		cursor: default;
	}
	.cam {
		font-size: 15px;
	}
	.spin {
		width: 15px;
		height: 15px;
		border-radius: 50%;
		border: 2px solid rgba(255, 255, 255, 0.4);
		border-top-color: #fff;
		animation: spin 0.7s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
	.editor {
		margin-top: 24px;
		max-width: 560px;
		display: flex;
		flex-direction: column;
		gap: 16px;
		padding: 22px 24px;
		border: 1px solid var(--k-border-1);
		border-radius: 12px;
		background: var(--k-surface);
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.field span {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--k-text-dim);
	}
	.field input,
	.field textarea {
		width: 100%;
		padding: 10px 12px;
		background: var(--k-surface-2, var(--k-surface));
		border: 1px solid var(--k-border-2);
		border-radius: 8px;
		color: var(--k-text);
		font-family: var(--k-font-sans);
		font-size: 14px;
		resize: vertical;
	}
	.field input:focus,
	.field textarea:focus {
		outline: none;
		border-color: var(--k-border-strong);
	}
	.editor-actions {
		display: flex;
		justify-content: flex-end;
		gap: 10px;
	}
	.cancel,
	.save {
		height: 40px;
		padding: 0 18px;
		border-radius: 8px;
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
	}
	.cancel {
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-3);
	}
	.save {
		background: var(--k-primary);
		border: none;
		color: var(--k-on-primary);
	}
	.save:disabled,
	.cancel:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.who {
		flex: 1;
		min-width: 260px;
	}
	.name-row {
		display: flex;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
	}
	h1 {
		font-size: 34px;
		margin: 0;
		color: var(--k-text-bright);
	}
	.pro {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.04em;
		color: var(--k-ongoing);
		background: rgba(95, 191, 126, 0.14);
		border: 1px solid rgba(95, 191, 126, 0.4);
		border-radius: 6px;
		padding: 3px 9px;
	}
	.handle {
		font-size: 14px;
		color: var(--k-text-dimmer);
		margin-top: 6px;
	}
	.bio {
		max-width: 560px;
		font-size: 14.5px;
		line-height: 1.6;
		color: var(--k-text-muted);
		margin: 14px 0 0;
	}
	.actions {
		display: flex;
		gap: 10px;
		flex-shrink: 0;
	}
	.edit {
		height: 42px;
		padding: 0 20px;
		border-radius: 8px;
		background: var(--k-primary);
		border: none;
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
	}
	.gear {
		width: 42px;
		height: 42px;
		border-radius: 8px;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-3);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.gear:hover {
		border-color: rgba(255, 255, 255, 0.34);
		color: var(--k-text);
	}
	.stats {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
		gap: 1px;
		margin-top: 36px;
		background: var(--k-border-1);
		border: 1px solid var(--k-border-1);
		border-radius: 12px;
		overflow: hidden;
	}
	.stat {
		background: var(--k-surface);
		padding: 20px 22px;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.stat-value {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 28px;
		color: var(--k-text-bright);
		line-height: 1;
	}
	.stat-label {
		font-size: 12.5px;
		color: var(--k-text-dimmer);
	}
	.body {
		display: grid;
		grid-template-columns: 1.6fr 1fr;
		gap: 44px;
		padding-top: 48px;
		padding-bottom: 20px;
		align-items: start;
	}
	.left,
	.right {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.left {
		gap: 40px;
	}
	.right {
		gap: 32px;
	}
	.section {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.section-label {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16px;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		margin: 0;
		color: var(--k-text-dimmer);
	}
	.reading-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.reading-row {
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 12px;
		border-radius: 10px;
		text-decoration: none;
		transition: background 0.12s;
	}
	.reading-row:hover {
		background: rgba(255, 255, 255, 0.035);
	}
	.mini-cover {
		flex: 0 0 auto;
		width: 48px;
		height: 68px;
		border-radius: 6px;
	}
	.reading-info {
		flex: 1;
		min-width: 0;
	}
	.reading-top {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 12px;
	}
	.reading-title {
		font-weight: 700;
		font-size: 14.5px;
		color: var(--k-text-1);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.reading-ch {
		flex: 0 0 auto;
		font-size: 12px;
		color: var(--k-text-faint);
	}
	.reading-genre {
		font-size: 12px;
		color: var(--k-text-fainter);
		margin-top: 3px;
	}
	.reading-bar {
		margin-top: 9px;
		height: 4px;
		border-radius: 3px;
		background: rgba(255, 255, 255, 0.1);
		overflow: hidden;
	}
	.reading-bar .fill,
	.genre-bar .fill {
		height: 100%;
		background: var(--k-primary);
		border-radius: 3px;
	}
	.tabs {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}
	.tab {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-size: 13.5px;
		font-weight: 700;
		padding: 8px 16px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: transparent;
		color: var(--k-text-dimmer);
		border: 1px solid var(--k-border-4);
		transition: all 0.15s;
	}
	.tab.on {
		background: var(--k-primary);
		color: var(--k-on-primary);
		border-color: var(--k-primary);
	}
	.count {
		opacity: 0.55;
		font-weight: 600;
	}
	.shelf-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
		gap: 22px 18px;
	}
	.shelf-card {
		display: flex;
		flex-direction: column;
		gap: 9px;
		text-decoration: none;
	}
	.cover {
		position: relative;
		width: 100%;
		aspect-ratio: 2 / 3;
		border-radius: 8px;
		transition: opacity 0.15s;
	}
	.shelf-card:hover .cover {
		opacity: 0.82;
	}
	.rating {
		position: absolute;
		top: 8px;
		right: 8px;
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-weight: 700;
		font-size: 11px;
		color: var(--k-text);
		background: rgba(12, 12, 13, 0.72);
		border-radius: 5px;
		padding: 3px 6px;
	}
	.shelf-title {
		font-weight: 600;
		font-size: 13px;
		color: var(--k-text-1);
		line-height: 1.25;
	}
	.shelf-sub {
		font-size: 11.5px;
		color: var(--k-text-faint);
	}
	.card {
		border: 1px solid var(--k-border-1);
		border-radius: 12px;
		padding: 22px 24px;
		background: var(--k-surface);
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.card-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 15px;
		margin: 0;
		color: var(--k-text);
	}
	.setting-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	.setting-text {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}
	.setting-label {
		font-weight: 600;
		font-size: 14px;
		color: var(--k-text-1);
	}
	.setting-desc {
		font-size: 12.5px;
		color: var(--k-text-faint);
		line-height: 1.4;
	}
	.switch {
		flex: 0 0 auto;
		width: 44px;
		height: 26px;
		border-radius: 999px;
		border: 1px solid var(--k-border-4);
		background: var(--k-border-1);
		padding: 0;
		cursor: pointer;
		position: relative;
		transition:
			background 0.15s,
			border-color 0.15s;
	}
	.switch:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.switch.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
	}
	.switch .knob {
		position: absolute;
		top: 2px;
		left: 2px;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		background: var(--k-on-primary, #fff);
		transition: transform 0.15s;
	}
	.switch.on .knob {
		transform: translateX(18px);
	}
	.setting-error {
		margin: 4px 0 0;
		font-size: 12.5px;
		color: var(--k-danger, #e08a8a);
	}
	.genres {
		display: flex;
		flex-direction: column;
		gap: 13px;
	}
	.genre {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.genre-top {
		display: flex;
		justify-content: space-between;
		font-size: 13px;
	}
	.gname {
		color: var(--k-text-2);
		font-weight: 600;
	}
	.gpct {
		color: var(--k-text-faint);
	}
	.genre-bar {
		height: 6px;
		border-radius: 4px;
		background: var(--k-border-1);
		overflow: hidden;
	}
	.activity {
		display: flex;
		flex-direction: column;
	}
	.act-row {
		display: flex;
		gap: 14px;
		align-items: flex-start;
		padding: 14px 0;
		border-top: 1px solid var(--k-border);
	}
	.act-icon {
		flex: 0 0 auto;
		width: 30px;
		height: 30px;
		border-radius: 8px;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.act-text {
		font-size: 13.5px;
		color: var(--k-text-2);
		line-height: 1.45;
	}
	.act-time {
		font-size: 11.5px;
		color: var(--k-text-fainter);
		margin-top: 3px;
	}
	@media (max-width: 820px) {
		.body {
			grid-template-columns: 1fr;
			gap: 32px;
		}
	}
</style>
