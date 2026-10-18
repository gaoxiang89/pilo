import { ChevronDown } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { AgentKind } from "@/lib/agent";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "@/ui";

const AGENTS: readonly AgentKind[] = ["pi", "droid"];

/**
 * 新会话落地页上的代理选择。Desktop 与 Remote WebUI 共用。
 * 已创建的会话不能在这里切换。
 */
export function AgentPicker({
	agent,
	onChange,
	disabled = false,
}: {
	agent: AgentKind;
	onChange: (agent: AgentKind) => void;
	disabled?: boolean;
}) {
	const { t } = useTranslation();
	const label =
		agent === "droid"
			? t("connection.defaultAgentDroid")
			: t("connection.defaultAgentPi");
	return (
		<DropdownMenu>
			<DropdownMenuTrigger asChild>
				<button
					type="button"
					disabled={disabled}
					className="group flex h-7 w-fit max-w-[40%] min-w-0 items-center gap-1.5 rounded-lg border border-foreground/[0.10] bg-background px-2 text-xs text-muted-foreground outline-hidden transition-colors hover:bg-muted/40 hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-60 dark:border-input-border/70"
					aria-label={t("connection.defaultAgent")}
				>
					<span className="min-w-0 flex-1 truncate text-left">{label}</span>
					<ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
				</button>
			</DropdownMenuTrigger>
			<DropdownMenuContent align="start" className="min-w-36">
				{AGENTS.map((candidate) => (
					<DropdownMenuItem key={candidate} onClick={() => onChange(candidate)}>
						<span className="min-w-0 flex-1 truncate">
							{candidate === "droid"
								? t("connection.defaultAgentDroid")
								: t("connection.defaultAgentPi")}
						</span>
						{agent === candidate ? (
							<span className="shrink-0 text-xs text-muted-foreground">
								{t("common.current")}
							</span>
						) : null}
					</DropdownMenuItem>
				))}
			</DropdownMenuContent>
		</DropdownMenu>
	);
}
