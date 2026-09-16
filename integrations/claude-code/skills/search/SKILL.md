---
name: search
description: Returns raw mdya hybrid-search hits against the local Markdown corpus, with no LLM synthesis. User-facing only — invoke directly via /mdya:search <query>.
disable-model-invocation: true
allowed-tools: mcp__plugin_mdya_mdya__search
---

# search

## Steps

Call `mdya:search` once. Only `query` comes from the user; everything else stays at its default (`mode="hybrid"`, `k=20`, `level="doc"`, `collections=[]`).

Present the results in score order, unchanged:

- Header: `total = N / top score = X.XXX` (when hits = 0, top score is `-`)
- Top 3 hits: `<rank>. <collection> / <path>  (score: X.XXX, matched_chunks: N)`, with the snippet on the next line as `   <snippet>`
- Hit 4 onward: `<rank>. <collection> / <path>  (score: X.XXX)` only

When hits = 0, print the header line only.

## Do not

- Synthesize, summarize, generate citations, print a "no results" template, or suggest fallbacks
- Supplement with knowledge sources other than mdya
