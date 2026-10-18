/** 会话使用的编码代理。连接只记住新建会话的默认值。 */
export type AgentKind = "pi" | "droid";

export function connectionDefaultAgent(
	connection: { defaultAgent?: string | null } | null | undefined,
): AgentKind {
	return connection?.defaultAgent === "droid" ? "droid" : "pi";
}

export function agentFromSessionPath(
	sessionPath: string | null | undefined,
): AgentKind {
	const path = (sessionPath ?? "").replaceAll("\\", "/");
	return path.includes("/.factory/sessions/") ? "droid" : "pi";
}

/**
 * Factory 路径优先于索引里的 agent 字段。旧索引在加列之前会把 Droid 会话记成 Pi，
 * 但文件仍在 `~/.factory/sessions`。尚未落盘的草稿只看显式选择。
 */
export function resolveSessionAgent(session: {
	agent?: string | null;
	sessionPath?: string | null;
}): AgentKind {
	if (agentFromSessionPath(session.sessionPath) === "droid") return "droid";
	if (session.agent === "droid" || session.agent === "pi") return session.agent;
	return "pi";
}

export function omitSlashSuggestions<T extends { kind: string }>(
	items: readonly T[],
): T[] {
	return items.filter((item) => item.kind !== "command");
}
