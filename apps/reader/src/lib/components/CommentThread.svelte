<script lang="ts">
	import { page } from '$app/state';
	import { auth } from '$lib/auth.svelte';
	import {
		socialLive,
		loadChapterComments,
		submitChapterComment,
		loadSeriesComments,
		submitSeriesComment,
		canModerate,
		deleteChapterComment,
		banCommenter,
		type CommentView,
	} from '$lib/data/social-repo';
	import Icon from '$lib/components/Icon.svelte';

	interface Props {
		/** 'chapter' for a per-chapter thread, 'series' for series-level discussion. */
		targetType: 'chapter' | 'series';
		/** Backend id of the target (chapter id or series id). */
		targetId: string;
		/** Stable key for the offline/local fallback store. */
		storageKey: string;
		/** Composer placeholder + sign-in prompt context. */
		prompt?: string;
	}
	let {
		targetType,
		targetId,
		storageKey,
		prompt = 'Share your thoughts…',
	}: Props = $props();

	let comments = $state<CommentView[]>([]);
	let commentSort = $state<'top' | 'newest'>('newest');
	let revealed = $state<Record<string, boolean>>({});
	let draft = $state('');
	let spoiler = $state(false);
	let posting = $state(false);
	let postError = $state<string | null>(null);
	let modError = $state<string | null>(null);

	const needsAuth = $derived(socialLive() && !auth.user);
	const myInitial = $derived((auth.user?.username ?? 'K').charAt(0).toUpperCase());
	const canPost = $derived(draft.trim().length > 0 && !posting);
	const canMod = $derived(canModerate());
	const sortedComments = $derived(
		commentSort === 'top' ? [...comments].sort((a, b) => b.likes - a.likes) : comments,
	);

	const load = (id: string, key: string) =>
		targetType === 'series' ? loadSeriesComments(id, key) : loadChapterComments(id, key);
	const submit = (id: string, key: string, body: string, sp: boolean, cur: CommentView[]) =>
		targetType === 'series'
			? submitSeriesComment(id, key, body, sp, cur)
			: submitChapterComment(id, key, body, sp, cur);

	// (Re)load when the target or sign-in state changes.
	$effect(() => {
		const id = targetId;
		const key = storageKey;
		void auth.user?.id; // reload after sign-in so "mine" resolves
		let cancelled = false;
		if (socialLive() && !id) {
			comments = [];
			return;
		}
		load(id ?? '', key)
			.then((list) => {
				if (!cancelled) {
					comments = list;
					revealed = {};
				}
			})
			.catch(() => {});
		return () => {
			cancelled = true;
		};
	});

	function toggleLike(id: string) {
		comments = comments.map((c) =>
			c.id === id ? { ...c, liked: !c.liked, likes: c.likes + (c.liked ? -1 : 1) } : c,
		);
	}
	function reveal(id: string) {
		revealed = { ...revealed, [id]: true };
	}
	async function post() {
		const body = draft.trim();
		if (!body || posting) return;
		posting = true;
		postError = null;
		try {
			comments = await submit(targetId ?? '', storageKey, body, spoiler, comments);
			draft = '';
			spoiler = false;
			commentSort = 'newest';
		} catch (err) {
			postError = err instanceof Error ? err.message : 'Could not post your comment.';
		} finally {
			posting = false;
		}
	}
	async function removeComment(id: string) {
		modError = null;
		try {
			comments = await deleteChapterComment(id, comments);
		} catch (err) {
			modError = err instanceof Error ? err.message : 'Could not delete the comment.';
		}
	}
	async function banAuthor(c: CommentView) {
		modError = null;
		if (!confirm(`Ban ${c.name}? They won't be able to sign in.`)) return;
		try {
			await banCommenter(c.authorId);
			comments = comments.filter((x) => x.authorId !== c.authorId);
		} catch (err) {
			modError = err instanceof Error ? err.message : 'Could not ban the user.';
		}
	}
</script>

<div class="c-head">
	<h3><Icon name="comment" size={20} stroke="#87857f" />{comments.length} comments</h3>
	<div class="sort-tabs">
		<button class="stab" class:on={commentSort === 'top'} onclick={() => (commentSort = 'top')}>
			Top
		</button>
		<button
			class="stab"
			class:on={commentSort === 'newest'}
			onclick={() => (commentSort = 'newest')}
		>
			Newest
		</button>
	</div>
</div>

{#if needsAuth}
	<div class="signin-prompt">
		<span>Sign in to join the discussion.</span>
		<a
			class="signin-link"
			href={`/login?redirect=${encodeURIComponent(page.url.pathname + page.url.search)}`}
		>
			Sign in
		</a>
	</div>
{:else}
	<div class="composer">
		<div class="avatar me">{myInitial}</div>
		<div class="composer-body">
			<textarea bind:value={draft} placeholder={prompt} rows="3"></textarea>
			{#if postError}<p class="post-error">{postError}</p>{/if}
			<div class="composer-foot">
				<label class="spoiler-toggle">
					<input type="checkbox" bind:checked={spoiler} />
					<span>Contains spoilers</span>
				</label>
				<button class="post" class:enabled={canPost} onclick={post} disabled={!canPost}>
					{posting ? 'Posting…' : 'Post comment'}
				</button>
			</div>
		</div>
	</div>
{/if}

{#if modError}<p class="post-error">{modError}</p>{/if}

<div class="c-list">
	{#each sortedComments as c (c.id)}
		<div class="comment">
			<div class="avatar" style="background:{c.bg};color:{c.fg}">{c.initial}</div>
			<div class="c-body">
				<div class="c-meta">
					<span class="c-name">{c.name}</span>
					{#if c.isOp}<span class="op">TRANSLATOR</span>{/if}
					{#if c.mine}<span class="mine">You</span>{/if}
					<span class="c-time">{c.time}</span>
				</div>
				{#if c.hasSpoiler && !revealed[c.id]}
					<button class="spoiler-veil" onclick={() => reveal(c.id)}>
						<Icon name="alert" size={14} />Spoiler — tap to reveal
					</button>
				{:else}
					<p class="c-text">{c.body}</p>
				{/if}
				<div class="c-actions">
					<button class="like" class:on={c.liked} onclick={() => toggleLike(c.id)}>
						<Icon name="heart" size={15} fill={c.liked ? 'currentColor' : 'none'} />{c.likes}
					</button>
					<button class="reply"><Icon name="reply" size={15} />Reply</button>
					{#if canMod}
						<button class="mod" onclick={() => removeComment(c.id)}>
							<Icon name="x" size={14} />Delete
						</button>
						{#if !c.mine && c.authorId}
							<button class="mod danger" onclick={() => banAuthor(c)}>
								<Icon name="alert" size={14} />Ban
							</button>
						{/if}
					{/if}
				</div>
			</div>
		</div>
	{/each}
	{#if comments.length === 0}
		<p class="c-empty">No comments yet — be the first to start the discussion.</p>
	{/if}
</div>

<style>
	.c-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		margin-bottom: 18px;
	}
	.c-head h3 {
		display: flex;
		align-items: center;
		gap: 9px;
		margin: 0;
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 18px;
		color: var(--k-text-bright);
	}
	.sort-tabs {
		display: flex;
		gap: 4px;
	}
	.stab {
		font-size: 12.5px;
		font-weight: 600;
		padding: 6px 12px;
		border-radius: var(--k-radius-pill);
		border: 1px solid var(--k-border-4);
		background: transparent;
		color: var(--k-text-dimmer);
		cursor: pointer;
	}
	.stab.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.signin-prompt {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		flex-wrap: wrap;
		padding: 16px 18px;
		border: 1px solid var(--k-border-1);
		border-radius: 12px;
		background: var(--k-surface);
		color: var(--k-text-dimmer);
		font-size: 14px;
		margin-bottom: 20px;
	}
	.signin-link {
		font-weight: 700;
		color: var(--k-primary);
		text-decoration: none;
	}
	.composer {
		display: flex;
		gap: 14px;
		margin-bottom: 26px;
	}
	.avatar {
		flex: 0 0 auto;
		width: 40px;
		height: 40px;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		font-weight: 700;
		font-size: 15px;
	}
	.avatar.me {
		background: var(--k-primary);
		color: var(--k-on-primary);
	}
	.composer-body {
		flex: 1;
		min-width: 0;
	}
	textarea {
		width: 100%;
		box-sizing: border-box;
		resize: vertical;
		background: var(--k-surface);
		border: 1px solid var(--k-border-4);
		border-radius: 10px;
		padding: 12px 14px;
		color: var(--k-text-1);
		font: inherit;
		font-size: 14px;
		line-height: 1.5;
	}
	textarea:focus {
		outline: none;
		border-color: var(--k-primary);
	}
	.post-error {
		color: var(--k-danger, #e08a8a);
		font-size: 12.5px;
		margin: 8px 0 0;
	}
	.composer-foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		margin-top: 10px;
	}
	.spoiler-toggle {
		display: flex;
		align-items: center;
		gap: 7px;
		font-size: 13px;
		color: var(--k-text-dimmer);
		cursor: pointer;
	}
	.post {
		padding: 9px 18px;
		border-radius: 8px;
		border: none;
		background: var(--k-border-1);
		color: var(--k-text-faint);
		font-weight: 700;
		font-size: 13.5px;
		cursor: not-allowed;
	}
	.post.enabled {
		background: var(--k-primary);
		color: var(--k-on-primary);
		cursor: pointer;
	}
	.c-list {
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.c-empty {
		color: var(--k-text-faint);
		font-size: 14px;
		margin: 0;
	}
	.comment {
		display: flex;
		gap: 14px;
	}
	.c-body {
		flex: 1;
		min-width: 0;
	}
	.c-meta {
		display: flex;
		align-items: center;
		gap: 9px;
		flex-wrap: wrap;
	}
	.c-name {
		font-weight: 700;
		font-size: 14px;
		color: var(--k-text-1);
	}
	.op,
	.mine {
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.03em;
		padding: 2px 6px;
		border-radius: 5px;
	}
	.op {
		color: var(--k-ongoing, #5fbf7e);
		background: rgba(95, 191, 126, 0.14);
	}
	.mine {
		color: var(--k-primary);
		background: rgba(255, 255, 255, 0.08);
	}
	.c-time {
		font-size: 12px;
		color: var(--k-text-fainter);
	}
	.c-text {
		margin: 6px 0 0;
		font-size: 14.5px;
		line-height: 1.55;
		color: var(--k-text-2);
	}
	.spoiler-veil {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		margin-top: 8px;
		padding: 8px 12px;
		border-radius: 8px;
		border: 1px dashed var(--k-border-4);
		background: transparent;
		color: var(--k-text-dimmer);
		font-size: 13px;
		cursor: pointer;
	}
	.c-actions {
		display: flex;
		align-items: center;
		gap: 16px;
		margin-top: 10px;
	}
	.c-actions button {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		background: transparent;
		border: none;
		color: var(--k-text-faint);
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
		padding: 0;
	}
	.like.on {
		color: var(--k-primary);
	}
	.mod {
		color: var(--k-text-dimmer);
	}
	.mod.danger {
		color: var(--k-danger, #e08a8a);
	}
</style>
