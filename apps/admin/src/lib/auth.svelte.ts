/**
 * Admin auth state. Same shape as the reader's store, but gated on `isAdmin`:
 * the console only admits admin accounts. Persists the bearer token and injects
 * it into the backend so the admin mutations carry it.
 */
import type { Session } from '@komika/api';
import { backend } from './context';

const TOKEN_KEY = 'komika-admin-token';

type User = Session['user'];

export const auth = $state<{ user: User | null; ready: boolean }>({
	user: null,
	ready: false,
});

function readToken(): string | null {
	try {
		return localStorage.getItem(TOKEN_KEY);
	} catch {
		return null;
	}
}

function persist(token: string | null): void {
	try {
		if (token) localStorage.setItem(TOKEN_KEY, token);
		else localStorage.removeItem(TOKEN_KEY);
	} catch {
		/* private mode / quota */
	}
	backend.setToken?.(token);
}

/** Restore + validate a persisted admin session. */
export async function initAuth(): Promise<void> {
	if (auth.ready) return;
	const token = readToken();
	if (token) {
		backend.setToken?.(token);
		try {
			const session = await backend.session();
			// Only keep the session if it's actually an admin. A resolved-but-
			// invalid/non-admin token is cleared here; a genuine bad token also
			// resolves to null on the next successful round-trip.
			auth.user = session?.user?.isAdmin ? session.user : null;
			if (!auth.user) persist(null);
		} catch {
			// Transient failure (backend unreachable): stay logged out but keep
			// the token so a blip doesn't force a full re-login (server always
			// re-validates). Matches the reader's initAuth.
			auth.user = null;
		}
	}
	auth.ready = true;
}

export async function login(username: string, password: string): Promise<void> {
	const session = await backend.login(username, password);
	if (!session.user.isAdmin) {
		// Not an admin — drop the session we just created and refuse.
		backend.setToken?.(session.token);
		try {
			await backend.logout();
		} catch {
			/* ignore */
		}
		backend.setToken?.(null);
		throw new Error('This account does not have admin access.');
	}
	persist(session.token);
	auth.user = session.user;
}

export async function logout(): Promise<void> {
	try {
		await backend.logout();
	} catch {
		/* ignore */
	}
	persist(null);
	auth.user = null;
}
