# JOB t440 Tiel — finish Forge v3 corrector + empty-content path

Repo: `/home/cesarops/wreckhunter2000-1/cesarops-forge-v3`
Also mirror if you edit: `/home/cesarops/repos/Forge`

Do not cap tokens. Think as long as you need. Close `</think>` and put the patch in content.

## Already done

- `decide_empty_answer_after_think` + loop_engine slap
- Tiel/Ornith match `reasoning_relaxed` (do not rematch `3b`)
- `corrector_tool_fix = true` in cluster_config
- `max_generation_tokens = 131072`

## You do

1. `src/loop_tuning.rs` Default: `corrector_tool_fix: true` (was false).
2. `src/inference_client.rs`: if `max_tokens == 0`, OMIT `max_tokens` from the JSON body (llama.cpp 0 = generate nothing). Keep merge_reasoning when content empty.
3. Usefulness / dual-coder empty: if `content` is empty and `reasoning_content` is non-empty, treat as a think-in-progress, NOT grade F / both_empty. Find `parallel_dual_coders:both_empty` and the usefulness halt. Merge reasoning into the coder text before grading.
4. Do not change NIM cloud caps.

## Pass

```
cd /home/cesarops/wreckhunter2000-1/cesarops-forge-v3
cargo test --offline empty_answer unclosed_think merge_reasoning
```

Reply with a unified diff only after `</think>`.
