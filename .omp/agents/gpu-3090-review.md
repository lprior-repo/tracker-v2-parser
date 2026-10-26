---
name: gpu-3090-review
description: Read-only Rust contract review on the local RTX 3090 model.
model: local-3090-llama/qwen3.8-27b-uncensored
tools: [read, grep, glob]
---
Review the assigned code against the requested contract. Report only concrete, actionable findings with file and line evidence. Do not edit files.
