import { type Dispatch, type SetStateAction, useCallback, useRef } from "react";

import { resolveSessionAgent } from "@/lib/agent";
import type { ChatSession } from "@/components/chat/chat-page-utils";
import type { createChatSessionClient } from "@/lib/api/chat-session-client";
import type {
	PiAgentState,
	PiModel,
	PiThinkingLevel,
} from "@/lib/api/pi-runtime";

type ChatSessionClient = ReturnType<typeof createChatSessionClient>;

type UseChatRuntimeConfigurationOptions = {
	session: ChatSession;
	client: ChatSessionClient;
	selectedModel: PiModel | null;
	selectedThinkingLevel: PiThinkingLevel;
	setSelectedModel: Dispatch<SetStateAction<PiModel | null>>;
	setSelectedThinkingLevel: Dispatch<SetStateAction<PiThinkingLevel>>;
	setThinkingLevels: Dispatch<SetStateAction<PiThinkingLevel[]>>;
};

export function useChatRuntimeConfiguration({
	session,
	client,
	selectedModel,
	selectedThinkingLevel,
	setSelectedModel,
	setSelectedThinkingLevel,
	setThinkingLevels,
}: UseChatRuntimeConfigurationOptions) {
	const initialConfigAppliedRef = useRef(new Set<string>());

	return useCallback(
		async (agentState: PiAgentState) => {
			if (resolveSessionAgent(session) === "droid") {
				if (initialConfigAppliedRef.current.has(session.id)) return;
				let state = agentState;
				if (
					session.initialModel &&
					selectedModel?.id === session.initialModel.id &&
					selectedModel.provider === session.initialModel.provider &&
					(state.model?.id !== session.initialModel.id ||
						state.model?.provider !== session.initialModel.provider)
				) {
					await client.setPiModel(session.initialModel);
					state = await client.getPiAgentState();
				}
				// The model switch above resets Droid's level, so apply the
				// landing choice after it.
				const level = session.initialThinkingLevel;
				if (
					level &&
					state.thinkingLevel !== level &&
					state.model?.thinkingLevels?.includes(level)
				) {
					await client.setPiThinkingLevel(level);
					state = await client.getPiAgentState();
				}
				if (state.model) setSelectedModel(state.model);
				setThinkingLevels([...(state.model?.thinkingLevels ?? [])]);
				setSelectedThinkingLevel(state.thinkingLevel);
				initialConfigAppliedRef.current.add(session.id);
				return;
			}
			if (session.sessionPath) {
				let configChanged = false;
				if (
					selectedModel &&
					(agentState.model?.provider !== selectedModel.provider ||
						agentState.model?.id !== selectedModel.id)
				) {
					await client.setPiModel(selectedModel);
					configChanged = true;
				}
				if (agentState.thinkingLevel !== selectedThinkingLevel) {
					await client.setPiThinkingLevel(selectedThinkingLevel);
					configChanged = true;
				}
				if (configChanged) {
					const [state, thinking] = await Promise.all([
						client.getPiAgentState(),
						client.getAvailablePiThinkingLevels(),
					]);
					setSelectedModel(state.model);
					setSelectedThinkingLevel(state.thinkingLevel);
					setThinkingLevels(thinking.levels);
				}
				return;
			}

			if (initialConfigAppliedRef.current.has(session.id)) return;
			let configChanged = false;
			if (
				session.initialModel &&
				(agentState.model?.provider !== session.initialModel.provider ||
					agentState.model?.id !== session.initialModel.id)
			) {
				await client.setPiModel(session.initialModel);
				configChanged = true;
			}
			if (
				session.initialThinkingLevel &&
				agentState.thinkingLevel !== session.initialThinkingLevel
			) {
				await client.setPiThinkingLevel(session.initialThinkingLevel);
				configChanged = true;
			}
			if (configChanged) {
				const [state, thinking] = await Promise.all([
					client.getPiAgentState(),
					client.getAvailablePiThinkingLevels(),
				]);
				setSelectedModel(state.model);
				setSelectedThinkingLevel(state.thinkingLevel);
				setThinkingLevels(thinking.levels);
			}
			initialConfigAppliedRef.current.add(session.id);
		},
		[
			client,
			selectedModel,
			selectedThinkingLevel,
			session,
			setSelectedModel,
			setSelectedThinkingLevel,
			setThinkingLevels,
		],
	);
}
