import type { PiModel } from "@/lib/api/pi-runtime";

export type DroidModelCache = {
	models: PiModel[];
	current: PiModel | null;
};

/** `droid.models` result: the catalog and Droid's current model. */
export type DroidModelCatalog = {
	models: PiModel[];
	model: PiModel | null;
};

/**
 * Probe the catalog. Without `thinking` only the current model carries its
 * thinking levels (about 6 s); with it every model does (about 20 s).
 */
export type DroidModelLoader = (
	thinking: boolean,
) => Promise<DroidModelCatalog>;

// The catalog belongs to the Factory account on a connection, not a project.
// It is persisted so the landing picker is not empty after a restart.
const STORAGE_KEY = "pilo.droidModels.v1";

const caches = new Map<string, DroidModelCache>();
const pending = new Map<string, Promise<DroidModelCache>>();
const pendingThinking = new Map<string, Promise<void>>();
const listeners = new Map<string, Set<(cache: DroidModelCache) => void>>();
// Connections probed during this run; persisted entries may be stale.
const fresh = new Set<string>();
const thinkingFresh = new Set<string>();
let hydrated = false;

const emptyCache: DroidModelCache = { models: [], current: null };

function hydrate() {
	if (hydrated) return;
	hydrated = true;
	try {
		const stored = window.localStorage.getItem(STORAGE_KEY);
		if (!stored) return;
		const parsed = JSON.parse(stored) as Record<string, DroidModelCache>;
		for (const [connectionId, cache] of Object.entries(parsed)) {
			if (Array.isArray(cache?.models)) {
				caches.set(connectionId, {
					models: cache.models,
					current: cache.current ?? null,
				});
			}
		}
	} catch {
		// Storage is a convenience; an empty cache is still correct.
	}
}

function persist() {
	try {
		window.localStorage.setItem(
			STORAGE_KEY,
			JSON.stringify(Object.fromEntries(caches)),
		);
	} catch {
		// Private windows and blocked storage keep the in-memory cache only.
	}
}

/** Keep thinking levels learned earlier for models the new list lacks them on. */
function withKnownThinking(
	model: PiModel,
	previous: readonly PiModel[] | undefined,
): PiModel {
	if (model.thinkingLevels) return model;
	const known = previous?.find(
		(candidate) =>
			candidate.id === model.id && candidate.provider === model.provider,
	);
	if (!known?.thinkingLevels) return model;
	return {
		...model,
		reasoning: known.reasoning,
		thinkingLevels: known.thinkingLevels,
		defaultThinkingLevel: known.defaultThinkingLevel,
	};
}

export function readDroidModelCache(connectionId: string): DroidModelCache {
	hydrate();
	return caches.get(connectionId) ?? emptyCache;
}

export function subscribeDroidModels(
	connectionId: string,
	listener: (cache: DroidModelCache) => void,
): () => void {
	let set = listeners.get(connectionId);
	if (!set) {
		set = new Set();
		listeners.set(connectionId, set);
	}
	set.add(listener);
	return () => {
		set.delete(listener);
	};
}

export function rememberDroidModels(
	connectionId: string,
	models: readonly PiModel[],
	current: PiModel | null,
) {
	if (!connectionId) return;
	hydrate();
	const previous = caches.get(connectionId);
	const next: DroidModelCache = {
		models:
			models.length > 0
				? models.map((model) => withKnownThinking(model, previous?.models))
				: (previous?.models ?? []),
		current: current
			? withKnownThinking(current, previous?.models)
			: (previous?.current ?? null),
	};
	caches.set(connectionId, next);
	if (models.length > 0) fresh.add(connectionId);
	persist();
	for (const listener of listeners.get(connectionId) ?? []) listener(next);
}

function loadThinking(connectionId: string, load: DroidModelLoader) {
	if (pendingThinking.has(connectionId)) return;
	const request = load(true)
		.then((catalog) => {
			thinkingFresh.add(connectionId);
			rememberDroidModels(connectionId, catalog.models, catalog.model);
		})
		.catch((error: unknown) => {
			console.warn("Failed to read Droid thinking levels", error);
		})
		.finally(() => {
			pendingThinking.delete(connectionId);
		});
	pendingThinking.set(connectionId, request);
}

/**
 * Load the Droid catalog before a Droid session exists.
 *
 * The quick probe resolves the promise; every model's thinking levels follow
 * in the background and reach `subscribeDroidModels` listeners. Without
 * `force`, a catalog already probed in this run is reused; one only restored
 * from storage is shown by the caller and refreshed here. Concurrent callers
 * share one probe; each probe starts a short-lived Droid process.
 */
export function loadDroidModels(
	connectionId: string,
	load: DroidModelLoader,
	force = false,
): Promise<DroidModelCache> {
	const cached = readDroidModelCache(connectionId);
	if (!force && fresh.has(connectionId) && cached.models.length > 0) {
		if (!thinkingFresh.has(connectionId)) loadThinking(connectionId, load);
		return Promise.resolve(cached);
	}
	const running = pending.get(connectionId);
	if (running) return running;
	const request = load(false).then((catalog) => {
		rememberDroidModels(connectionId, catalog.models, catalog.model);
		loadThinking(connectionId, load);
		return readDroidModelCache(connectionId);
	});
	pending.set(connectionId, request);
	const clear = () => {
		if (pending.get(connectionId) === request) pending.delete(connectionId);
	};
	void request.then(clear, clear);
	return request;
}
