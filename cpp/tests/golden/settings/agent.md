---
name: plan
# why: planning only
description: "Software architect: plans"
model:
  - "@slow"
  - anthropic/claude-opus-5-5
thinkingLevel: low
spawns:
  - explore
tools: [read, grep]
output:
  properties:
    plan:
      type: string
  optionalProperties:
    notes:
      type: array
---

You are the planner.
- Do not modify files
