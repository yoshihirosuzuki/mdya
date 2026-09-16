---
name: query
description: Answers questions grounded in a local Markdown corpus indexed by mdya. Use when the user asks about content in their indexed Markdown — design docs, decision records, release notes, personal notes, or any indexed Markdown content.
allowed-tools: Agent
---

# query

## Steps

### Step 1: Receive the question

If the question is ambiguous, ask once for clarification, then proceed.

### Step 2: Launch the subagent

Use the `Agent` tool with `subagent_type: "mdya:query-agent"`. Wrap the prompt as: "Generate an answer to: '<the user's question>'." Wrapping the question as an instruction suppresses the subagent's own "is a search needed?" pre-judgement while keeping the question text in context, so the RAG generation phase holds. If the user named a collection, include it. Query construction, retrieval, and generation are all owned by the subagent.

### Step 3: Present the subagent's result

Present the subagent's return value to the user as-is. Do not wrap it in a template or rewrite it.

## Do not

- Call `mdya:*` MCP tools directly from the main thread
- Launch the subagent more than once per request
- Supplement with knowledge sources other than mdya (web, training data)
- Run the `mdya` CLI via Bash (always go through the MCP tools)
