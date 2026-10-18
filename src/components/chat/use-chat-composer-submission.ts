import { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import {
	createChatSubmission,
	type ChatSubmission,
} from "@/lib/chat/chat-submission";
import type { ChatUiStatePatch } from "@/components/app/chat-ui-state-cache";

type PersistUiState = (key: string, patch: ChatUiStatePatch) => void;

export type UseChatComposerSubmissionOptions = {
	uiStateKey?: string | null;
	writeUiState?: PersistUiState;
	initialDeferredHistory: readonly string[];
	externalRunning?: boolean;
	historySubmissionBlocked: boolean;
	running: boolean;
	clearDraft: () => void;
	clearImages: () => void;
	onNewChat?: () => void;
	/** Droid 会话只保留 /new，其余斜杠命令按普通消息发送。 */
	piCommands?: boolean;
	compact: (customInstructions?: string) => unknown;
	tryExecuteExtensionCommand: (command: string) => Promise<boolean | null>;
	handleSubmit: (submission: ChatSubmission) => void;
	handleFollowUp: (submission: ChatSubmission) => void;
};

export function useChatComposerSubmission({
	uiStateKey,
	writeUiState,
	initialDeferredHistory,
	externalRunning = false,
	historySubmissionBlocked,
	running,
	clearDraft,
	clearImages,
	onNewChat,
	piCommands = true,
	compact,
	tryExecuteExtensionCommand,
	handleSubmit,
	handleFollowUp,
}: UseChatComposerSubmissionOptions) {
	const { t } = useTranslation();
	const pendingHistorySubmissionsRef = useRef<string[]>([
		...initialDeferredHistory,
	]);

	const persistDeferredHistorySubmissions = useCallback(
		(submissions: string[]) => {
			if (uiStateKey && writeUiState) {
				writeUiState(uiStateKey, { deferredSubmissions: submissions });
			}
		},
		[uiStateKey, writeUiState],
	);

	const tryHandleComposerCommand = useCallback(
		async (submission: ChatSubmission) => {
			const command = submission.text.trim();
			// 仅输入开头的斜杠作为命令执行，避免正文中的 / 误触发。
			if (!submission.text.startsWith("/")) return false;
			const commandName = command.slice(1).split(/\s+/, 1)[0];

			if (commandName === "new") {
				clearDraft();
				clearImages();
				onNewChat?.();
				return true;
			}
			if (!piCommands) return false;
			if (externalRunning) {
				toast.info(t("chat.externalReadOnly"));
				return true;
			}
			if (commandName === "compact") {
				if (running) {
					toast.info(t("chat.waitCompaction"));
					return true;
				}
				const customInstructions = command.slice("/compact".length).trim();
				clearDraft();
				clearImages();
				void compact(customInstructions || undefined);
				return true;
			}
			const executed = await tryExecuteExtensionCommand(command);
			// null 表示失败已提示；保留输入，也不回退成普通消息。
			if (executed === null) return true;
			if (executed) {
				clearDraft();
				clearImages();
				return true;
			}
			return false;
		},
		[
			clearDraft,
			clearImages,
			piCommands,
			compact,
			onNewChat,
			running,
			externalRunning,
			tryExecuteExtensionCommand,
			t,
		],
	);

	const handleComposerSubmit = useCallback(
		(submission: ChatSubmission) => {
			void (async () => {
				if (externalRunning && submission.text.trim() !== "/new") {
					toast.info(t("chat.externalReadOnly"));
					return;
				}
				if (await tryHandleComposerCommand(submission)) return;
				if (!historySubmissionBlocked) {
					handleSubmit(submission);
					clearImages();
					return;
				}
				if (submission.images.length > 0) {
					toast.info(t("chat.waitHistory"));
					return;
				}
				const trimmed = submission.text.trim();
				if (!trimmed) return;
				const next = [...pendingHistorySubmissionsRef.current, trimmed];
				pendingHistorySubmissionsRef.current = next;
				persistDeferredHistorySubmissions(next);
				clearDraft();
			})();
		},
		[
			clearDraft,
			clearImages,
			externalRunning,
			handleSubmit,
			historySubmissionBlocked,
			persistDeferredHistorySubmissions,
			tryHandleComposerCommand,
			t,
		],
	);

	useEffect(() => {
		if (externalRunning || historySubmissionBlocked || running) return;
		const [first, ...rest] = pendingHistorySubmissionsRef.current;
		if (!first) return;
		pendingHistorySubmissionsRef.current = [];
		persistDeferredHistorySubmissions([]);
		handleSubmit(createChatSubmission(first));
		for (const message of rest) handleFollowUp(createChatSubmission(message));
	}, [
		externalRunning,
		handleFollowUp,
		handleSubmit,
		historySubmissionBlocked,
		persistDeferredHistorySubmissions,
		running,
	]);

	return { handleComposerSubmit, tryHandleComposerCommand };
}
