import {
	type Dispatch,
	type RefObject,
	type SetStateAction,
	useCallback,
	useEffect,
	useRef,
	useState,
} from "react";

import {
	type ChatImageAttachment,
	type ChatSubmission,
	summarizeChatImages,
	toPiImageContents,
} from "@/lib/chat/chat-submission";
import { reduceConversationActions } from "@/lib/chat/conversation-replay";
import type {
	ConversationAction,
	ConversationState,
} from "@/lib/chat/conversation-types";
import type {
	PiAgentState,
	PiModel,
	PiThinkingLevel,
} from "@/lib/api/pi-runtime";
import {
	PI_PROMPT_RPC_TIMEOUT_MS,
	runtimeErrorMessage,
	type PiPromptResult,
} from "@/lib/api/pi-runtime";
import { chatRuntimeHasOpenTurn } from "@/lib/chat/chat-session-runtime-model";
import { randomId } from "@/lib/utils";
import { reducerContext } from "./remote-app-model";
import type {
	QueuedSubmission,
	RemoteSessionRpc,
} from "./use-remote-chat-transport";
import type { WebPiloClient } from "./web-pilo-client";

export type PendingProjectMcpPrompt = {
	sessionKey: string;
	clientMessageId: string;
	submission: ChatSubmission;
	shouldAutoTitle: boolean;
};

type UseRemoteChatActionsOptions = {
	client: WebPiloClient | null;
	activeSessionKey: string | null;
	activeSessionKeyRef: RefObject<string | null>;
	routeUiKey: string;
	composer: string;
	readOnly: boolean;
	hasSelectedSession: boolean;
	hasIdentifiedSession: boolean;
	droid?: boolean;
	autoTitleRequestedRef: RefObject<boolean>;
	pendingProjectMcpPromptsRef: RefObject<Map<string, PendingProjectMcpPrompt>>;
	projectMcpApprovalRequestedRef: RefObject<Set<string>>;
	queuedSubmissionsRef: RefObject<Map<string, QueuedSubmission>>;
	ensureDraftRuntime: () => Promise<void>;
	tryHandleComposerCommand: (submission: ChatSubmission) => Promise<boolean>;
	requestAutoTitle: (message: string) => void;
	clearComposer: () => void;
	handleComposerChange: (value: string) => void;
	setImages: Dispatch<SetStateAction<ChatImageAttachment[]>>;
	handleExpiredAuth: (error: unknown) => boolean;
	setFatalError: (message: string | null) => void;
	setConversation: Dispatch<SetStateAction<ConversationState>>;
	setAgentState: Dispatch<SetStateAction<PiAgentState | null>>;
	setDraftModel: Dispatch<SetStateAction<PiModel | null>>;
	setDraftThinkingLevel: Dispatch<SetStateAction<PiThinkingLevel | null>>;
	rpcForSession: RemoteSessionRpc;
};

export function useRemoteChatActions({
	client,
	activeSessionKey,
	activeSessionKeyRef,
	routeUiKey,
	composer,
	readOnly,
	hasSelectedSession,
	hasIdentifiedSession,
	droid = false,
	autoTitleRequestedRef,
	pendingProjectMcpPromptsRef,
	projectMcpApprovalRequestedRef,
	queuedSubmissionsRef,
	ensureDraftRuntime,
	tryHandleComposerCommand,
	requestAutoTitle,
	clearComposer,
	handleComposerChange,
	setImages,
	handleExpiredAuth,
	setFatalError,
	setConversation,
	setAgentState,
	setDraftModel,
	setDraftThinkingLevel,
	rpcForSession,
}: UseRemoteChatActionsOptions) {
	const [sendingSessionKeys, setSendingSessionKeys] = useState(
		new Set<string>(),
	);
	const sendingSessionKeysRef = useRef(new Set<string>());
	const sending =
		activeSessionKey !== null && sendingSessionKeys.has(activeSessionKey);
	const setSessionSending = useCallback(
		(sessionKey: string, value: boolean) => {
			if (value) sendingSessionKeysRef.current.add(sessionKey);
			else sendingSessionKeysRef.current.delete(sessionKey);
			setSendingSessionKeys((current) => {
				const next = new Set(current);
				if (value) next.add(sessionKey);
				else next.delete(sessionKey);
				return next;
			});
		},
		[],
	);
	const recoveredSubmissionsRef = useRef(new Map<string, ChatSubmission[]>());
	const [recoveryRevision, setRecoveryRevision] = useState(0);

	/* oxlint-disable react/set-state-in-effect, react/exhaustive-effect-dependencies -- Failed send-now submissions belong to their original session, including when it returns from the background. recoveryRevision 是重新回填输入框的显式触发代，不是 effect 读取的值。 */
	useEffect(() => {
		void recoveryRevision;
		const recovered = recoveredSubmissionsRef.current.get(routeUiKey);
		if (!recovered) return;
		recoveredSubmissionsRef.current.delete(routeUiKey);
		handleComposerChange(
			[composer, ...recovered.map((item) => item.text)]
				.filter(Boolean)
				.join("\n\n"),
		);
		setImages((current) => [
			...new Map(
				[...current, ...recovered.flatMap((item) => item.images)].map(
					(image) => [image.id, image],
				),
			).values(),
		]);
	}, [composer, handleComposerChange, recoveryRevision, routeUiKey, setImages]);
	/* oxlint-enable react/set-state-in-effect */

	const runLocalAction = useCallback(
		(action: ConversationAction) => {
			setConversation((current) =>
				reduceConversationActions(current, [action], reducerContext),
			);
		},
		[setConversation],
	);

	const sendPrompt = useCallback(
		async (
			targetSessionKey: string,
			submission: ChatSubmission,
			clientMessageId: string,
		) => {
			const result = await rpcForSession<PiPromptResult>(
				targetSessionKey,
				{
					type: "prompt",
					message: submission.text,
					images: toPiImageContents(submission.images),
				},
				PI_PROMPT_RPC_TIMEOUT_MS,
			);
			if (result?.disposition !== "handled") return true;
			const runtime = await client?.chatState(targetSessionKey);
			if (chatRuntimeHasOpenTurn(runtime)) return true;
			setConversation((current) =>
				activeSessionKeyRef.current === targetSessionKey
					? reduceConversationActions(
							current,
							[
								{ type: "local_user_queue_failed", clientMessageId },
								{ type: "local_turn_abort", timestampMs: Date.now() },
							],
							reducerContext,
						)
					: current,
			);
			return false;
		},
		[activeSessionKeyRef, client, rpcForSession, setConversation],
	);

	const sendSubmission = useCallback(
		async (
			submission: ChatSubmission,
			mode: "prompt" | "steer" | "follow_up",
		) => {
			if (!client || !activeSessionKey || readOnly) return;
			if (!submission.text.trim() && submission.images.length === 0) return;
			if (sendingSessionKeysRef.current.has(activeSessionKey)) return;
			const submissionSessionKey = activeSessionKey;
			const isActiveSession = () =>
				activeSessionKeyRef.current === submissionSessionKey;
			const shouldAutoTitle =
				mode === "prompt" &&
				!hasSelectedSession &&
				!autoTitleRequestedRef.current;
			const now = Date.now();
			const clientMessageId = randomId();
			const descriptors = summarizeChatImages(submission.images);
			let queuedClientMessageId: string | null = null;
			// Lock before the first await; React's disabled state may not have rendered yet.
			setSessionSending(submissionSessionKey, true);
			try {
				if (mode === "prompt" && (await tryHandleComposerCommand(submission)))
					return;
				// 乐观更新：与桌面端 beginTurn 对齐，先落本地 user / assistant-pending
				// 消息，再启动 runtime，避免 startChat 期间输入框毫无反馈。
				if (mode === "prompt") {
					runLocalAction({
						type: "local_user_submit",
						clientMessageId,
						text: submission.text,
						images: descriptors,
						timestampMs: now,
					});
					runLocalAction({
						type: "local_assistant_pending",
						timestampMs: now,
					});
				} else {
					queuedClientMessageId = clientMessageId;
					queuedSubmissionsRef.current.set(clientMessageId, {
						submission,
						queueKind: mode,
					});
					runLocalAction({
						type: "local_user_queue",
						clientMessageId,
						text: submission.text,
						images: descriptors,
						queueKind: mode,
						timestampMs: now,
					});
				}
				clearComposer();
				await ensureDraftRuntime();
				const accepted =
					mode === "prompt"
						? await sendPrompt(
								submissionSessionKey,
								submission,
								clientMessageId,
							)
						: (
								await rpcForSession<PiPromptResult>(submissionSessionKey, {
									type: mode,
									message: submission.text,
									images: toPiImageContents(submission.images),
								})
							)?.disposition !== "handled";
				if (!isActiveSession()) return;
				if (!accepted) {
					queuedSubmissionsRef.current.delete(clientMessageId);
					// `sendPrompt` already rolled back its own optimistic prompt; queued
					// steer/follow-up messages still need the local rollback here.
					if (mode !== "prompt") {
						runLocalAction({
							type: "local_user_queue_failed",
							clientMessageId,
						});
					}
					return;
				}
				if (shouldAutoTitle) requestAutoTitle(submission.text);
			} catch (error) {
				if (
					!queuedClientMessageId &&
					projectMcpApprovalRequestedRef.current.has(submissionSessionKey)
				) {
					// Keep the original prompt with its session while MCP approval restarts Pi.
					if (!pendingProjectMcpPromptsRef.current.has(submissionSessionKey)) {
						pendingProjectMcpPromptsRef.current.set(submissionSessionKey, {
							sessionKey: submissionSessionKey,
							clientMessageId,
							submission,
							shouldAutoTitle,
						});
					}
					return;
				}
				if (!isActiveSession()) {
					recoveredSubmissionsRef.current.set(routeUiKey, [
						...(recoveredSubmissionsRef.current.get(routeUiKey) ?? []),
						submission,
					]);
					setRecoveryRevision((value) => value + 1);
					return;
				}
				if (queuedClientMessageId) {
					queuedSubmissionsRef.current.delete(queuedClientMessageId);
					runLocalAction({
						type: "local_user_queue_failed",
						clientMessageId: queuedClientMessageId,
					});
				} else {
					runLocalAction({ type: "local_user_queue_failed", clientMessageId });
					runLocalAction({
						type: "conversation_runtime_error",
						message: runtimeErrorMessage(error),
						timestampMs: Date.now(),
					});
				}
				handleComposerChange(submission.text);
				setImages(submission.images);
				if (!handleExpiredAuth(error)) {
					setFatalError(error instanceof Error ? error.message : String(error));
				}
			} finally {
				setSessionSending(submissionSessionKey, false);
			}
		},
		[
			activeSessionKey,
			activeSessionKeyRef,
			autoTitleRequestedRef,
			clearComposer,
			client,
			ensureDraftRuntime,
			handleComposerChange,
			handleExpiredAuth,
			pendingProjectMcpPromptsRef,
			projectMcpApprovalRequestedRef,
			queuedSubmissionsRef,
			readOnly,
			requestAutoTitle,
			routeUiKey,
			rpcForSession,
			runLocalAction,
			sendPrompt,
			hasSelectedSession,
			tryHandleComposerCommand,
			setFatalError,
			setImages,
			setSessionSending,
		],
	);

	const handleEditQueued = useCallback(
		async (clientMessageId: string) => {
			if (!client || !activeSessionKey) return;
			const item = queuedSubmissionsRef.current.get(clientMessageId);
			if (!item) return;
			const remaining = [...queuedSubmissionsRef.current.entries()].filter(
				([id]) => id !== clientMessageId,
			);
			const targetSessionKey = activeSessionKey;
			const isActiveSession = () =>
				activeSessionKeyRef.current === targetSessionKey;
			setSessionSending(targetSessionKey, true);
			try {
				await rpcForSession<void>(targetSessionKey, { type: "clear_queue" });
				await remaining.reduce<Promise<void>>(
					(promise, [, queued]) =>
						promise.then(() =>
							rpcForSession<void>(targetSessionKey, {
								type: queued.queueKind,
								message: queued.submission.text,
								images: toPiImageContents(queued.submission.images),
							}),
						),
					Promise.resolve(),
				);
				if (!isActiveSession()) {
					recoveredSubmissionsRef.current.set(routeUiKey, [
						...(recoveredSubmissionsRef.current.get(routeUiKey) ?? []),
						item.submission,
					]);
					setRecoveryRevision((value) => value + 1);
					return;
				}
				queuedSubmissionsRef.current.delete(clientMessageId);
				runLocalAction({
					type: "local_user_queue_failed",
					clientMessageId,
				});
				handleComposerChange(item.submission.text);
				setImages(item.submission.images);
			} catch (error) {
				if (!handleExpiredAuth(error) && isActiveSession()) {
					setFatalError(error instanceof Error ? error.message : String(error));
				}
			} finally {
				setSessionSending(targetSessionKey, false);
			}
		},
		[
			activeSessionKey,
			activeSessionKeyRef,
			client,
			handleComposerChange,
			handleExpiredAuth,
			queuedSubmissionsRef,
			routeUiKey,
			rpcForSession,
			runLocalAction,
			setFatalError,
			setImages,
			setSessionSending,
		],
	);

	const sendNow = useCallback(
		async (submission: ChatSubmission, queuedClientMessageId?: string) => {
			if (
				!client ||
				!activeSessionKey ||
				readOnly ||
				activeSessionKeyRef.current !== activeSessionKey ||
				sendingSessionKeysRef.current.has(activeSessionKey)
			)
				return;
			const targetSessionKey = activeSessionKey;
			const isActiveSession = () =>
				activeSessionKeyRef.current === targetSessionKey;
			const runTargetAction = (action: ConversationAction) => {
				setConversation((current) =>
					isActiveSession()
						? reduceConversationActions(current, [action], reducerContext)
						: current,
				);
			};
			const clientMessageId = queuedClientMessageId ?? randomId();
			const queued = [...queuedSubmissionsRef.current.entries()];
			const remaining = queued.filter(([id]) => id !== clientMessageId);
			setSessionSending(targetSessionKey, true);
			if (!queuedClientMessageId) clearComposer();
			let queueCleared = false;
			let optimisticSubmitted = false;
			let promptSent = false;
			let replayedCount = 0;
			try {
				await rpcForSession<void>(targetSessionKey, { type: "clear_queue" });
				queueCleared = true;
				await rpcForSession<void>(targetSessionKey, { type: "abort" });
				runTargetAction({
					type: "local_turn_abort",
					timestampMs: Date.now(),
				});
				if (isActiveSession()) {
					for (const [id] of queued) {
						queuedSubmissionsRef.current.delete(id);
						runTargetAction({
							type: "local_user_queue_failed",
							clientMessageId: id,
						});
					}
				}

				const now = Date.now();
				if (isActiveSession()) {
					runTargetAction({
						type: "local_user_submit",
						clientMessageId,
						text: submission.text,
						images: summarizeChatImages(submission.images),
						timestampMs: now,
					});
					runTargetAction({
						type: "local_assistant_pending",
						timestampMs: now,
					});
					optimisticSubmitted = true;
				}
				const started = await sendPrompt(
					targetSessionKey,
					submission,
					clientMessageId,
				);
				promptSent = true;
				if (!started) {
					// `sendPrompt` already rolled back the optimistic prompt.
					if (remaining.length > 0) {
						recoveredSubmissionsRef.current.set(routeUiKey, [
							...(recoveredSubmissionsRef.current.get(routeUiKey) ?? []),
							...remaining.map(([, pending]) => pending.submission),
						]);
						setRecoveryRevision((value) => value + 1);
					}
					return;
				}

				if (isActiveSession()) {
					for (const [id, pending] of remaining) {
						queuedSubmissionsRef.current.set(id, pending);
						runTargetAction({
							type: "local_user_queue",
							clientMessageId: id,
							text: pending.submission.text,
							images: summarizeChatImages(pending.submission.images),
							queueKind: pending.queueKind,
							timestampMs: now,
						});
					}
				}
				await remaining.reduce<Promise<void>>(
					(promise, [, pending]) =>
						promise.then(async () => {
							await rpcForSession<void>(targetSessionKey, {
								type: pending.queueKind,
								message: pending.submission.text,
								images: toPiImageContents(pending.submission.images),
							});
							replayedCount += 1;
						}),
					Promise.resolve(),
				);
			} catch (error) {
				if (optimisticSubmitted && !promptSent) {
					runTargetAction({
						type: "local_user_queue_failed",
						clientMessageId,
					});
					runTargetAction({
						type: "local_turn_abort",
						timestampMs: Date.now(),
					});
				}
				// Keep acknowledged queue entries; recover the failed entry and its unsent tail.
				const failedQueued = promptSent
					? remaining.slice(replayedCount)
					: queueCleared
						? queued
						: [];
				if (isActiveSession()) {
					for (const [id] of failedQueued) {
						queuedSubmissionsRef.current.delete(id);
						runTargetAction({
							type: "local_user_queue_failed",
							clientMessageId: id,
						});
					}
				}
				const recovered = promptSent
					? failedQueued.map(([, pending]) => pending.submission)
					: queueCleared || !queuedClientMessageId
						? [
								submission,
								...failedQueued
									.filter(([id]) => id !== clientMessageId)
									.map(([, pending]) => pending.submission),
							]
						: [];
				if (recovered.length > 0) {
					recoveredSubmissionsRef.current.set(routeUiKey, [
						...(recoveredSubmissionsRef.current.get(routeUiKey) ?? []),
						...recovered,
					]);
					setRecoveryRevision((value) => value + 1);
				}
				if (!handleExpiredAuth(error) && isActiveSession()) {
					setFatalError(error instanceof Error ? error.message : String(error));
				}
			} finally {
				setSessionSending(targetSessionKey, false);
			}
		},
		[
			activeSessionKey,
			activeSessionKeyRef,
			clearComposer,
			client,
			handleExpiredAuth,
			queuedSubmissionsRef,
			readOnly,
			routeUiKey,
			rpcForSession,
			sendPrompt,
			setConversation,
			setFatalError,
			setSessionSending,
		],
	);

	const handleSendQueuedNow = useCallback(
		async (clientMessageId: string) => {
			const item = queuedSubmissionsRef.current.get(clientMessageId);
			if (!item) return;
			await sendNow(item.submission, clientMessageId);
		},
		[queuedSubmissionsRef, sendNow],
	);

	const handleSendNow = useCallback(
		async (submission: ChatSubmission) => {
			if (!client || !activeSessionKey || readOnly) return;
			if (!submission.text.trim() && submission.images.length === 0) return;
			if (await tryHandleComposerCommand(submission)) return;
			await sendNow(submission);
		},
		[activeSessionKey, client, readOnly, sendNow, tryHandleComposerCommand],
	);

	const resumeProjectMcpPrompt = useCallback(
		async (sessionKey: string) => {
			const pending = pendingProjectMcpPromptsRef.current.get(sessionKey);
			pendingProjectMcpPromptsRef.current.delete(sessionKey);
			projectMcpApprovalRequestedRef.current.delete(sessionKey);
			if (!pending || !client) return;

			const isActiveSession = () => activeSessionKeyRef.current === sessionKey;
			setSessionSending(sessionKey, true);
			try {
				const started = await sendPrompt(
					pending.sessionKey,
					pending.submission,
					pending.clientMessageId,
				);
				if (isActiveSession() && pending.shouldAutoTitle && started) {
					requestAutoTitle(pending.submission.text);
				}
			} catch (error) {
				if (isActiveSession()) {
					runLocalAction({
						type: "conversation_runtime_error",
						message: runtimeErrorMessage(error),
						timestampMs: Date.now(),
					});
					handleComposerChange(pending.submission.text);
					setImages(pending.submission.images);
					if (!handleExpiredAuth(error)) {
						setFatalError(
							error instanceof Error ? error.message : String(error),
						);
					}
				} else {
					console.error(
						"Failed to resume background MCP-approved prompt",
						error,
					);
				}
			} finally {
				setSessionSending(sessionKey, false);
			}
		},
		[
			activeSessionKeyRef,
			client,
			handleComposerChange,
			handleExpiredAuth,
			pendingProjectMcpPromptsRef,
			projectMcpApprovalRequestedRef,
			requestAutoTitle,
			runLocalAction,
			sendPrompt,
			setFatalError,
			setImages,
			setSessionSending,
		],
	);

	const failProjectMcpPrompt = useCallback(
		(sessionKey: string, error: unknown) => {
			const pending = pendingProjectMcpPromptsRef.current.get(sessionKey);
			pendingProjectMcpPromptsRef.current.delete(sessionKey);
			projectMcpApprovalRequestedRef.current.delete(sessionKey);
			if (!pending || activeSessionKey !== sessionKey) return;
			runLocalAction({
				type: "conversation_runtime_error",
				message: runtimeErrorMessage(error),
				timestampMs: Date.now(),
			});
			handleComposerChange(pending.submission.text);
			setImages(pending.submission.images);
		},
		[
			activeSessionKey,
			handleComposerChange,
			pendingProjectMcpPromptsRef,
			projectMcpApprovalRequestedRef,
			runLocalAction,
			setImages,
		],
	);

	const cancelProjectMcpPrompt = useCallback(
		(sessionKey: string) => {
			const pending = pendingProjectMcpPromptsRef.current.get(sessionKey);
			pendingProjectMcpPromptsRef.current.delete(sessionKey);
			projectMcpApprovalRequestedRef.current.delete(sessionKey);
			if (!pending || activeSessionKey !== sessionKey) return;
			runLocalAction({ type: "local_turn_abort", timestampMs: Date.now() });
		},
		[
			activeSessionKey,
			pendingProjectMcpPromptsRef,
			projectMcpApprovalRequestedRef,
			runLocalAction,
		],
	);

	const stop = useCallback(async () => {
		if (!client || !activeSessionKey) return;
		pendingProjectMcpPromptsRef.current.delete(activeSessionKey);
		projectMcpApprovalRequestedRef.current.delete(activeSessionKey);
		runLocalAction({ type: "local_turn_abort", timestampMs: Date.now() });
		await client
			.sendChatCommand(activeSessionKey, { type: "abort" })
			.catch(() => undefined);
	}, [
		activeSessionKey,
		client,
		pendingProjectMcpPromptsRef,
		projectMcpApprovalRequestedRef,
		runLocalAction,
	]);

	const changeModel = useCallback(
		async (model: PiModel) => {
			// A draft keeps the choice locally; starting the runtime here would
			// index an empty session. The first prompt applies it.
			if (!hasIdentifiedSession) {
				setDraftModel(model);
				setDraftThinkingLevel(model.defaultThinkingLevel ?? null);
				return;
			}
			if (!activeSessionKey) return;
			if (!hasIdentifiedSession) setDraftModel(model);
			const targetSessionKey = activeSessionKey;
			const isActiveSession = () =>
				activeSessionKeyRef.current === targetSessionKey;
			try {
				await ensureDraftRuntime();
				if (!isActiveSession()) return;
				const next = await rpcForSession<PiModel>(targetSessionKey, {
					type: "set_model",
					provider: model.provider,
					modelId: model.id,
				});
				if (droid) {
					// Droid swaps the thinking choices and level with the model.
					const state = await rpcForSession<PiAgentState>(targetSessionKey, {
						type: "get_state",
					});
					if (isActiveSession()) {
						setDraftModel(next);
						setAgentState(state);
					}
					return;
				}
				if (isActiveSession()) {
					setDraftModel(next);
					setAgentState((current) =>
						current
							? { ...current, model: next }
							: {
									model: next,
									thinkingLevel: "off",
									isStreaming: false,
									isCompacting: false,
									messageCount: 0,
									pendingMessageCount: 0,
								},
					);
				}
			} catch (error) {
				if (isActiveSession())
					setFatalError(error instanceof Error ? error.message : String(error));
			}
		},
		[
			activeSessionKey,
			activeSessionKeyRef,
			droid,
			ensureDraftRuntime,
			hasIdentifiedSession,
			rpcForSession,
			setAgentState,
			setDraftModel,
			setDraftThinkingLevel,
			setFatalError,
		],
	);

	const changeThinking = useCallback(
		async (level: PiThinkingLevel) => {
			if (!hasIdentifiedSession) {
				setDraftThinkingLevel(level);
				return;
			}
			if (!activeSessionKey) return;
			const targetSessionKey = activeSessionKey;
			const isActiveSession = () =>
				activeSessionKeyRef.current === targetSessionKey;
			try {
				await ensureDraftRuntime();
				if (!isActiveSession()) return;
				await rpcForSession<void>(targetSessionKey, {
					type: "set_thinking_level",
					level,
				});
				if (isActiveSession()) {
					setAgentState((current) =>
						current ? { ...current, thinkingLevel: level } : current,
					);
				}
			} catch (error) {
				if (isActiveSession())
					setFatalError(error instanceof Error ? error.message : String(error));
			}
		},
		[
			activeSessionKey,
			activeSessionKeyRef,
			ensureDraftRuntime,
			hasIdentifiedSession,
			rpcForSession,
			setAgentState,
			setDraftThinkingLevel,
			setFatalError,
		],
	);

	return {
		sending,
		sendSubmission,
		handleEditQueued,
		handleSendQueuedNow,
		handleSendNow,
		stop,
		changeModel,
		changeThinking,
		resumeProjectMcpPrompt,
		failProjectMcpPrompt,
		cancelProjectMcpPrompt,
	};
}
