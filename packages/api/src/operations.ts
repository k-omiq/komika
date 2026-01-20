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
		isCached
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
	query Search($query: String!, $page: Int) {
		search(query: $query, page: $page) {
			items {
				...SeriesFields
			}
			page
			hasNextPage
			total
		}
	}
`;

export const SERIES = /* GraphQL */ `
	${SERIES_FIELDS}
	query Series($id: ID!) {
		series(id: $id) {
			...SeriesFields
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
		}
	}
`;

export const MARK = /* GraphQL */ `
	${SERIES_FIELDS}
	mutation Mark($seriesId: ID!, $marked: Boolean!) {
		mark(seriesId: $seriesId, marked: $marked) {
			...SeriesFields
		}
	}
`;

export const SET_PROGRESS = /* GraphQL */ `
	mutation SetProgress($chapterId: ID!, $lastPageRead: Int!, $read: Boolean!) {
		setProgress(chapterId: $chapterId, lastPageRead: $lastPageRead, read: $read)
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
	query Comments($chapterId: ID!, $page: Int) {
		comments(chapterId: $chapterId, page: $page) {
			items {
				id
				chapterId
				author {
					...UserRefFields
				}
				body
				hasSpoiler
				createdAt
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
			chapterId
			author {
				...UserRefFields
			}
			body
			hasSpoiler
			createdAt
		}
	}
`;

// ---- auth ------------------------------------------------------------------

const SESSION_FIELDS = /* GraphQL */ `
	fragment SessionFields on Session {
		token
		user {
			id
			username
			avatarUrl
			isAdmin
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

export const BAN_USER = /* GraphQL */ `
	${USER_REF}
	mutation BanUser($userId: ID!, $banned: Boolean!) {
		banUser(userId: $userId, banned: $banned) {
			...UserRefFields
		}
	}
`;

export const DELETE_COMMENT = /* GraphQL */ `
	mutation DeleteComment($commentId: ID!) {
		deleteComment(commentId: $commentId)
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
