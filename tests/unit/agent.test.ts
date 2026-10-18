import assert from "node:assert/strict";
import test from "node:test";

import {
	agentFromSessionPath,
	connectionDefaultAgent,
	resolveSessionAgent,
} from "../../src/lib/agent.ts";

test("connection default agent stays Pi unless the connection stores droid", () => {
	assert.equal(connectionDefaultAgent(null), "pi");
	assert.equal(connectionDefaultAgent({}), "pi");
	assert.equal(connectionDefaultAgent({ defaultAgent: "pi" }), "pi");
	assert.equal(connectionDefaultAgent({ defaultAgent: "other" }), "pi");
	assert.equal(connectionDefaultAgent({ defaultAgent: "droid" }), "droid");
});

test("factory session paths select Droid on both slash styles", () => {
	assert.equal(
		agentFromSessionPath(
			"/Users/enoreyes/.factory/sessions/-Users-enoreyes-code-work/id.jsonl",
		),
		"droid",
	);
	assert.equal(
		agentFromSessionPath(
			"C:\\Users\\me\\.factory\\sessions\\-D--work\\id.jsonl",
		),
		"droid",
	);
	assert.equal(
		agentFromSessionPath("/Users/me/.pi/agent/sessions/--work--/id.jsonl"),
		"pi",
	);
});

test("an unsaved draft keeps its explicit agent, and a factory path overrides a stale Pi index", () => {
	assert.equal(resolveSessionAgent({ agent: "droid" }), "droid");
	assert.equal(resolveSessionAgent({ agent: "pi" }), "pi");
	assert.equal(
		resolveSessionAgent({
			agent: "pi",
			sessionPath: "/home/me/.factory/sessions/-work-/abc.jsonl",
		}),
		"droid",
	);
});
