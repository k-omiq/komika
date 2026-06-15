/**
 * GraphQL operation documents for Komika's unified backend.
 *
 * These are domain-shaped (they mirror `@komika/types`), so the client needs no
 * mapping layer — the server is responsible for federating Suwayomi (catalog,
 * chapters, pages, library, progress) and adding the social layer. The contract
 * these operations expect is documented in `schema/komika.graphql`.
 */

const SERIES_FIELDS = /* GraphQL */ `
	fragment SeriesFields on Series {
		id
		title
		altTitles
		author
		artist
		description
		genres
		type
		status
		coverUrl
		sourceId
		chapterCount
		isMarked
		isNsfw
		rating {
			average
			count
			distribution
		}
		scan {
			avgIntervalHours
			overrideIntervalHours
			pollEveryMinutes
			paused
			statusOverride
			pausedOverride
			pollEveryMinutesOverride
			lastScannedAt
			nextScanAt
		}
		createdAt
		updatedAt
	}
`;

const CHAPTER_FIELDS = /* GraphQL */ `
	fragment ChapterFields on Chapter {
		id
		seriesId
		number
		title
		pageCount
		uploadedAt
		scanlator
		read
		lastPageRead
		bookmarked
		isDownloaded
	}
`;

const USER_REF = /* GraphQL */ `
	fragment UserRefFields on UserRef {
		id
		username
		avatarUrl
	}
`;

// ---- catalog ---------------------------------------------------------------

export const DISCOVERY = /* GraphQL */ `
	${SERIES_FIELDS}
	query Discovery {
		discovery {
			kind
			title
			genre
			items {
				...SeriesFields
			}
		}
	}
`;

export const UPDATES = /* GraphQL */ `
	${SERIES_FIELDS}
	query Updates($page: Int) {
		updates(page: $page) {
			items {
				...SeriesFields
			}
			page
			hasNextPage
			total
		}
	}
`;

export const SEARCH = /* GraphQL */ `
	${SERIES_FIELDS}
	query Search(
		$query: String!
		$page: Int
		$genres: [String!]
		$minRating: Float
		$maxRating: Float
	) {
		search(
			query: $query
			page: $page
			genres: $genres
			minRating: $minRating
			maxRating: $maxRating
		) {
			items {
				...SeriesFields
			}
			page
			hasNextPage
			total
		}
	}
`;

// The full genre/tag facet set across the persisted catalogue (S4), most-common
// first with counts — drives the search genre multi-select.
export const GENRE_FACETS = /* GraphQL */ `
	query GenreFacets {
		genreFacets {
			genre
			count
		}
	}
`;

// Multi-source aggregated chapters for a work (S2): one row per chapter number
// across ALL a work's sources, with per-source availability for reading.
export const AGGREGATED_CHAPTERS = /* GraphQL */ `
	query AggregatedChapters($workId: ID!) {
		aggregatedChapters(workId: $workId) {
			number
			title
			sources {
				sourceType
				sourceId
				suwayomiMangaId
				chapterId
				scanlator
			}
		}
	}
`;

export const SERIES = /* GraphQL */ `
	${SERIES_FIELDS}
	query Series($id: ID!) {
		series(id: $id) {
			...SeriesFields
			libraryStatus
			isFavorite
			views {
				total
				last7d
				last24h
			}
		}
	}
`;

// Federated multi-extension search (S3): fans out to every installed source,
// dedupes to one canonical work per series, and returns each work with its
// per-source translator list. User-facing; NSFW-gated by the viewer's posture.
export const SEARCH_ALL_SOURCES = /* GraphQL */ `
	${SERIES_FIELDS}
	query SearchAllSources($query: String!, $page: Int) {
		searchAllSources(query: $query, page: $page) {
			items {
				series {
					...SeriesFields
				}
				translators {
					sourceType
					sourceId
					sourceName
					lang
					suwayomiMangaId
					extensionPkgName
					extensionIconUrl
				}
			}
			page
			hasNextPage
			sourcesQueried
		}
	}
`;

export const CHAPTERS = /* GraphQL */ `
	${CHAPTER_FIELDS}
	query Chapters($seriesId: ID!) {
		chapters(seriesId: $seriesId) {
			...ChapterFields
		}
	}
`;

export const PAGES = /* GraphQL */ `
	query Pages($chapterId: ID!) {
		pages(chapterId: $chapterId) {
			index
			sourceUrl
			width
			height
		}
	}
`;

// ---- library & progress ----------------------------------------------------

export const LIBRARY = /* GraphQL */ `
	${SERIES_FIELDS}
	query Library {
		library {
			...SeriesFields
			libraryStatus
			isFavorite
		}
	}
`;

// Batched per-series read progress for the whole library (one query instead of a
// `chapters()` fan-out per series) — shelves the Library/Profile screens fast.
export const LIBRARY_PROGRESS = /* GraphQL */ `
	query LibraryProgress {
		libraryProgress {
			id
			read
			total
		}
	}
`;

export const MARK = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation Mark($seriesId: ID!, $marked: Boolean!) {
		mark(seriesId: $seriesId, marked: $marked) {
			...SeriesFields
			libraryStatus
			isFavorite
		}
	}
`;

export const SET_LIBRARY_STATUS = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation SetLibraryStatus($seriesId: ID!, $status: String) {
		setLibraryStatus(seriesId: $seriesId, status: $status) {
			...SeriesFields
			libraryStatus
			isFavorite
		}
	}
`;

export const SET_FAVORITE = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation SetFavorite($seriesId: ID!, $favorite: Boolean!) {
		setFavorite(seriesId: $seriesId, favorite: $favorite) {
			...SeriesFields
			libraryStatus
			isFavorite
		}
	}
`;

export const SET_PROGRESS = /* GraphQL */ `
	mutation SetProgress($chapterId: ID!, $lastPageRead: Int!, $read: Boolean!) {
		setProgress(chapterId: $chapterId, lastPageRead: $lastPageRead, read: $read)
	}
`;

// Popularity signal: fire once per chapter open (see `views` module server-side). No
// auth — anonymous reads count too — so it never carries user identity.
export const RECORD_VIEW = /* GraphQL */ `
	mutation RecordView($seriesId: ID!) {
		recordView(seriesId: $seriesId)
	}
`;

// ---- social ----------------------------------------------------------------

export const REVIEWS = /* GraphQL */ `
	${USER_REF}
	query Reviews($seriesId: ID!, $page: Int) {
		reviews(seriesId: $seriesId, page: $page) {
			items {
				id
				seriesId
				author {
					...UserRefFields
				}
				score
				body
				hasSpoiler
				createdAt
				updatedAt
			}
			page
			hasNextPage
			total
		}
	}
`;

export const MY_REVIEW = /* GraphQL */ `
	${USER_REF}
	query MyReview($seriesId: ID!) {
		myReview(seriesId: $seriesId) {
			id
			seriesId
			author {
				...UserRefFields
			}
			score
			body
			hasSpoiler
			createdAt
			updatedAt
		}
	}
`;

export const POST_REVIEW = /* GraphQL */ `
	${USER_REF}
	mutation PostReview($input: PostReviewInput!) {
		postReview(input: $input) {
			id
			seriesId
			author {
				...UserRefFields
			}
			score
			body
			hasSpoiler
			createdAt
			updatedAt
		}
	}
`;

export const COMMENTS = /* GraphQL */ `
	${USER_REF}
	query Comments($targetType: String!, $targetId: ID!, $page: Int) {
		comments(targetType: $targetType, targetId: $targetId, page: $page) {
			items {
				id
				targetType
				targetId
				parentId
				author {
					...UserRefFields
				}
				body
				hasSpoiler
				mediaUrl
				mediaWidth
				mediaHeight
				createdAt
				likes
				dislikes
				myVote
			}
			page
			hasNextPage
			total
		}
	}
`;

export const POST_COMMENT = /* GraphQL */ `
	${USER_REF}
	mutation PostComment($input: PostCommentInput!) {
		postComment(input: $input) {
			id
			targetType
			targetId
			parentId
			author {
				...UserRefFields
			}
			body
			hasSpoiler
			mediaUrl
			mediaWidth
			mediaHeight
			createdAt
			likes
			dislikes
			myVote
		}
	}
`;

// Like (1), dislike (-1), or clear (0) a comment; returns the fresh tallies.
export const VOTE_COMMENT = /* GraphQL */ `
	mutation VoteComment($commentId: ID!, $value: Int!) {
		voteComment(commentId: $commentId, value: $value) {
			likes
			dislikes
			myVote
		}
	}
`;

// ---- notifications ---------------------------------------------------------

export const NOTIFICATIONS = /* GraphQL */ `
	${USER_REF}
	query Notifications($page: Int) {
		notifications(page: $page) {
			id
			kind
			actor {
				...UserRefFields
			}
			commentId
			commentExcerpt
			targetType
			targetId
			seriesId
			count
			createdAt
			read
		}
	}
`;

export const UNREAD_NOTIFICATION_COUNT = /* GraphQL */ `
	query UnreadNotificationCount {
		unreadNotificationCount
	}
`;

export const MARK_NOTIFICATIONS_READ = /* GraphQL */ `
	mutation MarkNotificationsRead($ids: [ID!]) {
		markNotificationsRead(ids: $ids)
	}
`;

// ---- auth ------------------------------------------------------------------

const SESSION_USER_FIELDS = /* GraphQL */ `
	fragment SessionUserFields on SessionUser {
		id
		username
		displayName
		bio
		avatarUrl
		isAdmin
		showNsfw
		joinedAt
	}
`;

const SESSION_FIELDS = /* GraphQL */ `
	${SESSION_USER_FIELDS}
	fragment SessionFields on Session {
		token
		user {
			...SessionUserFields
		}
	}
`;

export const SESSION = /* GraphQL */ `
	${SESSION_FIELDS}
	query Session {
		session {
			...SessionFields
		}
	}
`;

export const LOGIN = /* GraphQL */ `
	${SESSION_FIELDS}
	mutation Login($username: String!, $password: String!) {
		login(username: $username, password: $password) {
			...SessionFields
		}
	}
`;

export const REGISTER = /* GraphQL */ `
	${SESSION_FIELDS}
	mutation Register($input: RegisterInput!) {
		register(input: $input) {
			...SessionFields
		}
	}
`;

export const LOGOUT = /* GraphQL */ `
	mutation Logout {
		logout
	}
`;

// ---- admin -----------------------------------------------------------------

export const UPDATE_SERIES_ADMIN = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation UpdateSeriesAdmin($input: SeriesAdminInput!) {
		updateSeriesAdmin(input: $input) {
			...SeriesFields
		}
	}
`;

export const SCAN_STATUS = /* GraphQL */ `
	query ScanStatus {
		scanStatus {
			librarySize
			overdueCount
			lastTickAt
			nextDueAt
		}
	}
`;

export const TRIGGER_SCAN = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation TriggerScan($seriesId: ID!) {
		triggerScan(seriesId: $seriesId) {
			...SeriesFields
		}
	}
`;

const ADMIN_USER_FIELDS = /* GraphQL */ `
	fragment AdminUserFields on AdminUser {
		id
		username
		email
		avatarUrl
		isAdmin
		isBanned
		createdAt
	}
`;

export const BAN_USER = /* GraphQL */ `
	${ADMIN_USER_FIELDS}
	mutation BanUser($userId: ID!, $banned: Boolean!) {
		banUser(userId: $userId, banned: $banned) {
			...AdminUserFields
		}
	}
`;

export const DELETE_COMMENT = /* GraphQL */ `
	mutation DeleteComment($commentId: ID!) {
		deleteComment(commentId: $commentId)
	}
`;

export const USERS = /* GraphQL */ `
	${ADMIN_USER_FIELDS}
	query Users($page: Int) {
		users(page: $page) {
			items {
				...AdminUserFields
			}
			page
			hasNextPage
			total
		}
	}
`;

export const SET_USER_ADMIN = /* GraphQL */ `
	${ADMIN_USER_FIELDS}
	mutation SetUserAdmin($userId: ID!, $isAdmin: Boolean!) {
		setUserAdmin(userId: $userId, isAdmin: $isAdmin) {
			...AdminUserFields
		}
	}
`;

export const MERGE_QUEUE = /* GraphQL */ `
	query MergeQueue {
		mergeQueue {
			id
			sourceSeriesId
			candidateWorkId
			candidateTitle
			sourceTitle
			score
			method
			status
			createdAt
		}
	}
`;

export const RESOLVE_MERGE_CANDIDATE = /* GraphQL */ `
	mutation ResolveMergeCandidate($id: ID!, $accept: Boolean!) {
		resolveMergeCandidate(id: $id, accept: $accept)
	}
`;

export const MERGE_WORKS = /* GraphQL */ `
	mutation MergeWorks($sourceWorkId: ID!, $targetWorkId: ID!) {
		mergeWorks(sourceWorkId: $sourceWorkId, targetWorkId: $targetWorkId) {
			targetWorkId
			movedSourceSeries
		}
	}
`;

export const ADD_SOURCE_SERIES = /* GraphQL */ `
	mutation AddSourceSeries($suwayomiMangaId: ID!) {
		addSourceSeries(suwayomiMangaId: $suwayomiMangaId) {
			decision
			workId
			matchedWorkId
			score
			method
			sourceSeriesId
		}
	}
`;

// ---- admin sources & extensions (EXT-1/EXT-2) --------------------------------

const EXTENSION_FIELDS = /* GraphQL */ `
	fragment ExtensionFields on ExtensionInfo {
		pkgName
		name
		lang
		versionName
		isInstalled
		hasUpdate
		isNsfw
		iconUrl
		repo
	}
`;

export const EXTENSIONS = /* GraphQL */ `
	${EXTENSION_FIELDS}
	query Extensions($refresh: Boolean!) {
		extensions(refresh: $refresh) {
			...ExtensionFields
		}
	}
`;

export const SOURCES = /* GraphQL */ `
	query Sources {
		sources {
			id
			name
			lang
			isNsfw
			iconUrl
			pkgName
		}
	}
`;

export const SOURCE_BROWSE = /* GraphQL */ `
	query SourceBrowse($sourceId: ID!, $type: SourceBrowseType!, $page: Int!, $query: String) {
		sourceBrowse(sourceId: $sourceId, type: $type, page: $page, query: $query) {
			page
			hasNextPage
			items {
				suwayomiMangaId
				title
				thumbnailUrl
				inLibrary
			}
		}
	}
`;

export const ADD_EXTENSION_REPO = /* GraphQL */ `
	mutation AddExtensionRepo($indexUrl: String!) {
		addExtensionRepo(indexUrl: $indexUrl)
	}
`;

export const INSTALL_EXTENSION = /* GraphQL */ `
	${EXTENSION_FIELDS}
	mutation InstallExtension($pkgName: String!) {
		installExtension(pkgName: $pkgName) {
			...ExtensionFields
		}
	}
`;

export const UNINSTALL_EXTENSION = /* GraphQL */ `
	${EXTENSION_FIELDS}
	mutation UninstallExtension($pkgName: String!) {
		uninstallExtension(pkgName: $pkgName) {
			...ExtensionFields
		}
	}
`;

export const UPDATE_EXTENSION = /* GraphQL */ `
	${EXTENSION_FIELDS}
	mutation UpdateExtension($pkgName: String!) {
		updateExtension(pkgName: $pkgName) {
			...ExtensionFields
		}
	}
`;

export const BULK_ADD_SOURCE_SERIES = /* GraphQL */ `
	mutation BulkAddSourceSeries($suwayomiMangaIds: [ID!]!) {
		bulkAddSourceSeries(suwayomiMangaIds: $suwayomiMangaIds) {
			total
			succeeded
			failed
			newWorks
			autoMerged
			queuedForReview
			alreadyExisting
			entries {
				suwayomiMangaId
				error
				result {
					decision
					workId
					matchedWorkId
					score
					method
					sourceSeriesId
				}
			}
		}
	}
`;

export const SET_SHOW_NSFW = /* GraphQL */ `
	mutation SetShowNsfw($value: Boolean!) {
		setShowNsfw(value: $value)
	}
`;

// Admin maintenance: materialize the whole Suwayomi library into the DB cache
// (metadata synchronously; chapter lists in a background task). Returns the
// number of series persisted. Admin-gated server-side (require_admin).
export const PERSIST_CATALOGUE = /* GraphQL */ `
	mutation PersistCatalogue {
		persistCatalogue
	}
`;

// Admin maintenance: materialize every canonical work's cover into the DB
// (work_cover_blob) so the web reader serves covers from /covers/{id}.webp instead
// of the Cloudflare image Worker. Kicks off a polite background crawl and returns
// how many works are still uncached (queued) at start. Admin-gated (require_admin).
export const MATERIALIZE_CATALOGUE_COVERS = /* GraphQL */ `
	mutation MaterializeCatalogueCovers {
		materializeCatalogueCovers
	}
`;

// ---- profile ---------------------------------------------------------------

export const UPDATE_PROFILE = /* GraphQL */ `
	${SESSION_USER_FIELDS}
	mutation UpdateProfile($input: UpdateProfileInput!) {
		updateProfile(input: $input) {
			...SessionUserFields
		}
	}
`;

export const MY_ACTIVITY = /* GraphQL */ `
	query MyActivity($limit: Int) {
		myActivity(limit: $limit) {
			id
			kind
			targetType
			targetId
			createdAt
		}
	}
`;

export const CANONICAL_UPDATES = /* GraphQL */ `
	query CanonicalUpdates($page: Int) {
		canonicalUpdates(page: $page) {
			workId
			mangadexId
			title
			isNsfw
			coverUrl
			latestChapter
			latestChapterTitle
			latestAt
		}
	}
`;

// ---- canonical reader path -------------------------------------------------

// The two enrichment fields (S2) are opt-in resolver fields keyed on the canonical
// `w_` work id, so they're selected ONLY here (canonicalSeries), never on the shared
// SeriesFields fragment used by native/numeric-id queries.
export const CANONICAL_SERIES = /* GraphQL */ `
	${SERIES_FIELDS}
	query CanonicalSeries($workId: ID!) {
		canonicalSeries(workId: $workId) {
			...SeriesFields
			libraryStatus
			isFavorite
			views {
				total
				last7d
				last24h
			}
			localizedDescriptions {
				lang
				description
			}
			credits {
				role
				name
			}
			covers {
				fileName
				url
				thumbnailUrl
				lang
				volume
				isPrimary
			}
		}
	}
`;

export const CANONICAL_CHAPTERS = /* GraphQL */ `
	${CHAPTER_FIELDS}
	query CanonicalChapters($workId: ID!) {
		canonicalChapters(workId: $workId) {
			...ChapterFields
		}
	}
`;

export const CANONICAL_PAGES = /* GraphQL */ `
	query CanonicalPages($chapterId: ID!) {
		canonicalPages(chapterId: $chapterId) {
			index
			sourceUrl
			width
			height
		}
	}
`;

// ---- native-embedded-Suwayomi source routing -------------------------------

const WORK_SOURCE_FIELDS = /* GraphQL */ `
	fragment WorkSourceFields on WorkSource {
		sourceType
		sourceId
		sourceKey
		sourceUrl
		isNsfw
		lang
		extension {
			pkgName
			repoUrl
			apkName
			versionCode
			lang
		}
	}
`;

export const WORK_SOURCES = /* GraphQL */ `
	${WORK_SOURCE_FIELDS}
	query WorkSources($workId: ID!) {
		workSources(workId: $workId) {
			...WorkSourceFields
		}
	}
`;

export const WORK_SOURCES_BATCH = /* GraphQL */ `
	${WORK_SOURCE_FIELDS}
	query WorkSourcesBatch($workIds: [ID!]!) {
		workSourcesBatch(workIds: $workIds) {
			workId
			sources {
				...WorkSourceFields
			}
		}
	}
`;

// ---- admin catalogue provenance + scan pause (EXT-2) -------------------------

export const SERIES_SOURCES_BATCH = /* GraphQL */ `
	${WORK_SOURCE_FIELDS}
	query SeriesSourcesBatch($seriesIds: [ID!]!) {
		seriesSourcesBatch(seriesIds: $seriesIds) {
			seriesId
			workId
			sources {
				...WorkSourceFields
			}
		}
	}
`;

// ---- admin background source-ingest jobs (S1) --------------------------------

const SOURCE_INGEST_JOB_FIELDS = /* GraphQL */ `
	fragment SourceIngestJobFields on SourceIngestJob {
		id
		sourceId
		state
		pagesDone
		itemsSeen
		succeeded
		failed
		newWorks
		autoMerged
		queuedForReview
		alreadyExisting
		error
		startedAt
		finishedAt
	}
`;

export const SOURCE_INGEST_JOBS = /* GraphQL */ `
	${SOURCE_INGEST_JOB_FIELDS}
	query SourceIngestJobs($active: Boolean!) {
		sourceIngestJobs(active: $active) {
			...SourceIngestJobFields
		}
	}
`;

export const START_SOURCE_INGEST = /* GraphQL */ `
	${SOURCE_INGEST_JOB_FIELDS}
	mutation StartSourceIngest($sourceId: ID!) {
		startSourceIngest(sourceId: $sourceId) {
			...SourceIngestJobFields
		}
	}
`;

export const CANCEL_SOURCE_INGEST = /* GraphQL */ `
	${SOURCE_INGEST_JOB_FIELDS}
	mutation CancelSourceIngest($jobId: ID!) {
		cancelSourceIngest(jobId: $jobId) {
			...SourceIngestJobFields
		}
	}
`;

export const START_EXTENSION_INGEST = /* GraphQL */ `
	${SOURCE_INGEST_JOB_FIELDS}
	mutation StartExtensionIngest($pkgName: ID!) {
		startExtensionIngest(pkgName: $pkgName) {
			...SourceIngestJobFields
		}
	}
`;

export const CANCEL_EXTENSION_INGEST = /* GraphQL */ `
	${SOURCE_INGEST_JOB_FIELDS}
	mutation CancelExtensionIngest($pkgName: ID!) {
		cancelExtensionIngest(pkgName: $pkgName) {
			...SourceIngestJobFields
		}
	}
`;

export const SET_SERIES_PAUSED = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation SetSeriesPaused($seriesId: ID!, $paused: Boolean!) {
		setSeriesPaused(seriesId: $seriesId, paused: $paused) {
			...SeriesFields
		}
	}
`;

// ---- admin series-detail editor (metadata + chapter overrides + rescan) ------

export const SERIES_ADMIN_META = /* GraphQL */ `
	query SeriesAdminMeta($seriesId: ID!) {
		seriesAdminMeta(seriesId: $seriesId) {
			workId
			titleOverride
			descriptionOverride
			contentTypeOverride
			isNsfwOverride
			tags
			hasCuratedTags
		}
	}
`;

export const UPDATE_SERIES_METADATA = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation UpdateSeriesMetadata($input: SeriesMetadataInput!) {
		updateSeriesMetadata(input: $input) {
			...SeriesFields
		}
	}
`;

export const WORK_CHAPTERS_ADMIN = /* GraphQL */ `
	query WorkChaptersAdmin($workId: ID!) {
		workChaptersAdmin(workId: $workId) {
			number
			key
			sourceTitle
			titleOverride
			effectiveTitle
			hidden
			sourceCount
		}
	}
`;

export const SET_CHAPTER_OVERRIDE = /* GraphQL */ `
	mutation SetChapterOverride($input: ChapterOverrideInput!) {
		setChapterOverride(input: $input)
	}
`;

export const RESCAN_WORK = /* GraphQL */ `
	mutation RescanWork($workId: ID!) {
		rescanWork(workId: $workId)
	}
`;
