import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

export const REPOSITORY_URL = "https://github.com/gaoxiang89/pilo";
export const RELEASES_URL = `${REPOSITORY_URL}/releases`;

export function useReleaseNotes(
	version: string | null,
	fallbackNotes: string | null,
	enabled: boolean,
) {
	const [result, setResult] = useState<{
		version: string;
		notes: string | null;
		failed: boolean;
	} | null>(null);
	const tag = version
		? version.startsWith("v")
			? version
			: `v${version}`
		: "";
	const releaseUrl = version
		? `${RELEASES_URL}/tag/${encodeURIComponent(tag)}`
		: RELEASES_URL;

	useEffect(() => {
		if (!enabled) {
			// oxlint-disable-next-line react/set-state-in-effect -- Clear failures on close so reopening retries without a request loop.
			setResult((current) => (current?.failed ? null : current));
			return;
		}
		if (!version || result?.version === version) return;
		const controller = new AbortController();
		let active = true;
		const timeout = window.setTimeout(() => controller.abort(), 15_000);

		void fetch(
			`https://api.github.com/repos/gaoxiang89/pilo/releases/tags/${encodeURIComponent(tag)}`,
			{
				headers: { Accept: "application/vnd.github+json" },
				signal: controller.signal,
			},
		)
			.then(async (response) => {
				if (!response.ok) throw new Error(`GitHub Release: ${response.status}`);
				const data: unknown = await response.json();
				if (
					typeof data !== "object" ||
					data === null ||
					!("body" in data) ||
					(typeof data.body !== "string" && data.body !== null)
				) {
					throw new Error("Invalid GitHub Release body");
				}
				if (active) {
					setResult({
						version,
						notes: data.body?.trim() || null,
						failed: false,
					});
				}
			})
			.catch(() => {
				if (active) setResult({ version, notes: null, failed: true });
			})
			.finally(() => window.clearTimeout(timeout));

		return () => {
			active = false;
			window.clearTimeout(timeout);
			controller.abort();
		};
	}, [enabled, version, tag, result?.version]);

	const currentResult = result?.version === version ? result : null;
	const rawNotes = currentResult?.notes ?? fallbackNotes;
	// The release workflow appends installer instructions; keep them on GitHub only.
	const notes =
		rawNotes
			?.replace(/^(?:---[ \t]*\r?\n\s*)?### 下载安装[ \t]*\r?$[\s\S]*/m, "")
			.trim() || null;
	return {
		notes,
		loading: enabled && version !== null && currentResult === null,
		failed: currentResult?.failed ?? false,
		openRelease: () => {
			void openUrl(releaseUrl).catch((error) => {
				console.error("Failed to open GitHub Release", error);
			});
		},
	};
}
