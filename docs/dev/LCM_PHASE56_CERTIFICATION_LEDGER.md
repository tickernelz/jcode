# Native LCM Phase 5/6 Certification Ledger

- Certification date: 2026-07-24 UTC
- Branch: `feat/native-lcm-context-engine`
- Native LCM implementation base: `97cb23ba472c80023e0e8eac9e0ab557a3e771f4`
- Fixed promotion candidate: `01846a47a91aa1a74cd252fc305299c4cf2783f4`
- Candidate identity is commit-object bound, not branch-tip bound. During the
  final audit interval another process rebased the branch onto newer
  `origin/master`; the frozen object remains preserved at
  `safety/native-lcm-pre-rebase-20260724`. No result in this ledger certifies or
  promotes the later rebased tip.
- Certification-only harness commits in its ancestry: `6dc7361` (Windows durability cohort) and `fb5fe7c` (development installer version expectation).
- Production hardening after `a9d3488`: `60c7bf8` (opaque-secret classification), `9e99e00` (deterministic emergency path), and `01846a4` (restored derived-context materialization).
- External Hermes oracle revision: `main@23d5adc`
- Decision rule: **promote only when every promotion gate is `PASS`**. `FAIL` and `BLOCKED` both evaluate to false.
- Decision: **DO NOT PROMOTE**. Keep `compaction.engine = "rolling"`.

## Post-rebase integration record

The feature branch was subsequently rebased, without conflicts, from old base
`6e443c82e456a2e51015e5555a5fcb52f439d410` onto tickernelz
`origin/master` at `1a1ad68c439135fd2f76d2226dd9ca49f5ed82f1`. The rebased LCM code tip is
`da0daf49deac4eb2ba6e5125e26db2013495c7b5`; the old candidate remains
recoverable at `safety/native-lcm-pre-rebase-20260724` and annotated tag
`safety-native-lcm-pre-rebase-20260724`.

Rebase integrity checks found all 16 commits in the same order with `git
range-diff` reporting 16 exact `=` mappings, zero changed mappings, and no
deleted path relative to the new master. The local `master` ref remained
`6e443c82e456a2e51015e5555a5fcb52f439d410`, while `origin/master` remained the
recorded target. No master branch was checked out, reset, committed to, or
pushed. The rewritten feature branch has not been force-pushed.

Post-rebase validation is integration evidence, not a transfer of the frozen
`01846a4` promotion certification:

- `git diff --check`, `cargo fmt --all -- --check`, and a fully clean-target
  `cargo check --locked` for `jcode-base`, `jcode-app-core`, `jcode-tui`, and
  `jcode` passed.
- Native LCM regressions passed 39/39; compaction-core passed 18/18; the exact
  repeated-resume orphan regression passed 1/1.
- Two independently clean serial `jcode-app-core` rounds each passed
  `1016 passed; 0 failed; 4 ignored`.
- Supporting config/session/protocol/provider crates passed all executed tests.
- Full `jcode-base`, root `jcode`, and `jcode-tui` runs retained known red tests
  from the exact `origin/master` baseline, but introduced no new failing test
  name: base had the same 5 failures, root the same 4, and TUI had 8 failures
  versus master's 9.
- Two independent read-only rebase audits found no semantic integration or
  patch-preservation blocker. They correctly retain real rebased-SHA Windows,
  provider-backed canary, portability, resource, observation, and final audit
  as fresh certification work rather than reusing pre-rebase proof.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/rebase-20260724T042744Z
post-rebase tests SHA256(SHA256SUMS) = 46f40a0c084d5b28b3cf858bc49bf6b8bb8fcfb7b3f8f98547647e88695d88dd
post-rebase app-core SHA256(SHA256SUMS) = 5eb7f2e96140d5f51ba1cc8726c2873b3baa1c4b2256e1b58b418c8780b37bce
master base SHA256(SHA256SUMS) = c7ac7a8224104797daa642a52b8f5904e0c9167500171c72931be13a4df5b105
master root/TUI SHA256(SHA256SUMS) = 24708426b2c764dc8c25414513cc467164e690acbd010154ae6288ad235d9e95
full rebase evidence SHA256(REBASE_SHA256SUMS) = 2e40ca409ca4ffc0181f6007013fb718c98b755e197790f470425c4a546e787c
```

The old exact-SHA evidence below remains an auditable historical no-promotion
record only. Certification for the rebased candidate must start from the new
SHA, and `rolling` remains the required default and rollback path meanwhile.

## Invariants held fixed

The certification did not change the product thresholds or engine boundaries:

- Background compaction threshold: 80%.
- Critical recovery threshold: 95%.
- Fresh tail: 10 messages.
- `reactive | proactive | semantic` remain trigger modes.
- `rolling | lcm` remain mutually exclusive engines.
- `compaction.model` does not enable LCM.
- Provider-native compaction, rolling, and LCM are mutually exclusive.
- The raw session journal is canonical. The context graph is rebuildable derived state.
- Hermes Python and SQLite are comparison-only. Native Jcode LCM has no Python, Hermes, or SQLite runtime dependency.

## Next-candidate acceptance thresholds

These thresholds were fixed before the next promotion-candidate run. They may
only be changed by an explicit evidence review, never to turn a red run green.

- Preferred-route critical LCM completion: <= 15,000 ms, with zero
  `local:emergency` events in the strict canary.
- Full regression: zero failures in two consecutive `jcode-app-core` runs from
  the same initially clean isolated `JCODE_HOME`.
- Scheduler: zero starvation and queue p95 <= 500 ms for both priority classes.
- Warm-cache reuse: cache-read/input ratio no more than five percentage points
  below rolling on the identical provider corpus.
- Durable overhead: LCM snapshot + journal + derived graph bytes <= 1.25x
  rolling after normalizing for the identical canonical raw transcript.
- Reconnect residue: zero unexplained one-message session snapshots.
- Real compaction token ratio: aggregate post/pre <= 0.35 after at least four
  compactions; at least 95% of non-bootstrap events must save tokens.
- Observation: >= 30 continuous minutes, >= 100 completed turns, >= 10
  sessions, and zero fallback, stale publication, corruption, or starvation.

## Boolean-AND decision

| ID | Promotion gate | Status | Evidence-based reason |
|---|---|---:|---|
| G01 | Source formatting, focused regressions, binary build | PASS | At `01846a4`, formatting and binary build passed; serial compaction passed 75/75, compaction-core 18/18, Windows writer-lock 1/1, and the exact orphan regression 1/1. Adversarial secret tests cover provider, deterministic emergency, durable reload, and historical derived-context materialization. |
| G02 | Native 30-trace, two-cycle quality scorecard | PASS | Rolling and native LCM both recalled 150/150 active facts. Wilson 95% lower bound was 0.9750. False completion claims were zero. |
| G03 | External Hermes release, deterministic replay, and shared-corpus comparison | PASS | Official full release validation passed. Deterministic replay completed 60 runs with zero failures and 258/258 canaries. Shared two-cycle corpus passed. |
| G04 | Every configured provider/profile/model route | PASS | Named profile `sub2api-codex` passed credential, provider, and real tool smoke for `gpt-5.6-luna`, `gpt-5.6-sol`, and `gpt-5.6-terra`. |
| G05 | Strict isolated LCM canary on its preferred configured route | **PASS** | After a diagnostic run overlapped the full local suite and timed out, a predeclared no-competing-work campaign required 3/3 passes. All three preferred `sub2api-codex:gpt-5.6-sol` completions passed at 8,717 / 8,877 / 8,787 ms with 24/24 exact replies, graph publication, rolling raw recovery, and zero emergency/fallback events. |
| G06 | Rolling rollback and canonical raw-history recovery | PASS | Both normal rollback and post-SIGKILL downgrade recovered exact planted values from raw history. |
| G07 | Scheduler fairness, starvation, and queue p95 | PASS | 24 background plus 24 critical jobs completed. Starved jobs: 0. Queue p95: 57.323 ms background and 31.342 ms critical, below 500 ms. |
| G08 | Durability, fault, migration, stale-result, mutation, and process-crash campaign | **BLOCKED** | The formal 12-case campaign and later rolling raw-history recovery passed. A socket-owner `SIGKILL` was issued 58 ms after `LCM_SCHEDULER`, but the retained chronology shows the in-flight client completed before the explicit restart. This supports listener/socket-owner kill handling and subsequent rolling recovery, not automatic post-restart recovery of the killed process's turn or graph publication. |
| G09 | Full repository regression gate | **PASS** | At exact candidate `01846a4`, two consecutive independently clean serial `jcode-app-core` rounds each passed `1013 passed; 0 failed; 4 ignored`. Exploratory parallel failures remain classified as process-global environment interference and are not substituted for deterministic evidence. |
| G10 | Windows compile, contention, and crash recovery | **PASS** | GitHub-hosted run `30064063308` at exact SHA `01846a4` passed x64 build plus 8/8 named LCM durability tests, 2/2 real-binary lifecycle tests, launch/install, and ARM64 build/launch/install. The cohort covers writer contention, stale writer, torn/corrupt/glued journal recovery, downgrade/rebuild, failed durable write, stale source, process exit/rebind, and client/server operation. |
| G11 | Unconfigured provider/API/account/profile portability | **BLOCKED** | Direct OpenRouter and Gemini OAuth/API-key identities still lack configured accounts. Catalog discovery found no suitable AI-model access tool (`99af593a-a6ee-4e83-b1c5-047e31aac9ae`); capability-gap suggestion `4351cf14-a85e-4ec1-bbdb-0b1430dcbbc9` was filed. |
| G12 | Sustained resource-growth bounds | **FAIL** | Final frozen campaigns passed token saving, durable growth, exactness, and orphan bounds but failed unchanged cache/fallback bounds. The 20-pair interleaved run had rolling/LCM warm cache ratios 0.9656/0.7688 (delta -19.69 pp), 20/17 cache-hit requests, and one LCM emergency event. The earlier four-session candidate run also failed cache delta at -26.95 pp. |
| G13 | Fixed observation window | **PASS** | The fresh systemd-backed campaign ran 1,800 seconds and completed 100/100 exact turns across 10 sessions. All 10 triggered compactions used exact effective route `sub2api-codex:gpt-5.6-sol`; emergency, fallback, corruption, starvation, stale publication, and orphan counts were zero; queue p95 was 0 ms. |
| G14 | Independent evidence-only audit | **PASS** | A final independent correction re-audit recomputed representative manifests, exact test selections/counts, route identities, thresholds, source hardening, raw process-kill chronology, G01-G15 statuses, and the Boolean. It confirmed the corrected G08 `BLOCKED` language is evidence-faithful. Valid G08/G11 `BLOCKED` and G12 `FAIL` outcomes remain unchanged; promotion remains false. |
| G15 | Live default and isolation safety | PASS | A redacted check found explicit `compaction.engine = "rolling"`. Canary homes/sockets/configs were isolated. No configured credential value was intentionally copied into certification artifacts. Artifacts retain synthetic secret-shaped test fixtures and review transcripts, so they are not a guarantee of containing no secret-like literals. |

The promotion Boolean is:

```text
PROMOTE = G01 ∧ G02 ∧ G03 ∧ G04 ∧ G05 ∧ G06 ∧ G07 ∧ G08
        ∧ G09 ∧ G10 ∧ G11 ∧ G12 ∧ G13 ∧ G14 ∧ G15
        = false
```

The immutable final status and critical evidence-manifest digest set is sealed at:

```text
$JCODE_SCRATCH_DIR/lcm-phase56/final-certification-hash-set-20260724T044000Z
SHA256(SHA256SUMS) = 9edc2e350bbfb36a4706fcff04af10fb7d67047a215f12d1846dce68d0d4a587
```

## Quality evidence

### Native rolling versus native LCM

Command:

```bash
JCODE_LCM_CERT_OUTPUT="$OUT/native-quality-scorecard.json" \
  cargo test -p jcode-base --lib \
  compaction::tests::lcm_synthetic_thirty_trace_scorecard_preserves_planted_facts \
  -- --exact --test-threads=1
```

Thresholds and results:

| Metric | Threshold | Rolling | Native LCM |
|---|---:|---:|---:|
| Active fact recall | >= 0.95 | 1.000 | 1.000 |
| Wilson 95% lower bound | >= 0.95 | 0.9750 | 0.9750 |
| Output/source character ratio | <= 0.35 | 0.1207 | 0.1207 |
| Latency p50 | recorded | 2.232 ms | 4.588 ms |
| Latency p95 | <= 1,000 ms | 3.318 ms | 6.191 ms |
| False completion claims | 0 | 0 | 0 |
| Compactions | >= 60 | 60 | 60 |

Artifact:

```text
$JCODE_SCRATCH_DIR/lcm-phase56/final-focused-20260724T004151Z
SHA256(SHA256SUMS) = fa23e2b45d27598389b22fea9c3de190290b9211898c8620095d444620b17d22
```

This ratio is character-based, not a fabricated token ratio.

### External Hermes shared corpus

Command:

```bash
python3 "$JCODE_SCRATCH_DIR/lcm-phase56/run-hermes-shared-scorecard.py" "$OUT"
```

The external runner uses the same neutral 30-trace, two-cycle generator contract. Canaries appear only in cycle 0, so final recall requires cross-cycle retention. Results: active and retrieval recall 1.0, Wilson lower bound 0.9750, 60 compressions, zero failures, zero false completions, p50 22.316 ms, p95 24.405 ms. Hermes reported token ratio 0.4286. Its token ratio and Jcode's character ratio are different units and are not compared as if interchangeable.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/hermes-shared-quality-20260724T003100Z
SHA256(SHA256SUMS) = 2490e077d38fc7ea25a42ba4792476d6a43ad03027419befe8fc4b1e3309fcd7
runner SHA256 = aa3a180059e94f89446d85a68c7b3e85fd8807d5a5b45187bc4c32925b92f81d
```

### External Hermes release and deterministic replay

Commands:

```bash
cd /home/zhafron/.jcode/scratch/lcm-eval/hermes-lcm
scripts/validate_release.sh --full --keep-going --output "$OUT"

python3 scripts/lcm_benchmark.py \
  --fixture-dir "$FIXTURE_DIR" \
  --policy benchmarks/policies/baseline_272k.yaml \
  --policy benchmarks/policies/codex_gpt_long_context.yaml \
  --output "$OUT" --allow-external-output --json
```

The official release script passed diff, compile, shell syntax, focused/full/low-fd pytest, benchmark smoke, and smoke/release stress. The deterministic replay executed 30 fixtures x 2 policies = 60 runs, with zero failures and 258/258 active and retrieval canaries.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/hermes-release-full-20260724T000624Z
SHA256(SHA256SUMS) = 9994dd52f3804dd41292ce6b44f2850554cab0a3c53371d239397f43e988f564

$JCODE_SCRATCH_DIR/lcm-phase56/hermes-deterministic-20260724T000553Z
SHA256(SHA256SUMS) = b16cb1744edc82bdc204a1888ee57c67dad16bd2a299fb1084bf52d8348cdf2f
```

## Provider and identity evidence

Command pattern, once per configured model:

```bash
target/debug/jcode auth-test \
  --provider-profile sub2api-codex \
  --model "$MODEL" \
  --json --output "$OUT/$MODEL.json"
```

The tool smoke is enabled by default. All three configured models passed. The direct run session `session_hedgehog_1784851840629_cca1d2d7fdde196e` persisted `provider_key=sub2api-codex` and `model=gpt-5.6-sol`. No secret value is in this ledger.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/live-provider-matrix-final-20260724T001856Z
SHA256(SHA256SUMS) = b5c891bb838bf13834ada44c7c810bc422d4d361c462ac03244265c285c110ee
```

Coverage status:

| Route | Status | Reason |
|---|---:|---|
| `sub2api-codex/gpt-5.6-luna` | PASS | Credential + provider + real tool smoke. |
| `sub2api-codex/gpt-5.6-sol` | PASS | Credential + provider + real tool smoke and persisted exact profile/model identity. |
| `sub2api-codex/gpt-5.6-terra` | PASS | Credential + provider + real tool smoke. |
| Direct OpenRouter provider pin/account | BLOCKED | No independently configured credential/account route. |
| Gemini OAuth/CLI identity | BLOCKED | No configured account/auth route. |
| Gemini API-key identity | BLOCKED | No configured credential route. |
| All other unconfigured provider/API/account/profile combinations | BLOCKED | Absence of configuration is not a pass. |

Fixes proven by exact one-test commands:

```bash
cargo test -p jcode --lib \
  cli::commands::tests::ndjson_compaction_event_preserves_complete_lcm_telemetry \
  -- --exact --test-threads=1
cargo test -p jcode --lib \
  cli::auth_test::named_profile_tests::auth_test_preserves_explicit_named_compatible_profile \
  -- --exact --test-threads=1
cargo test -p jcode-provider-openrouter-runtime --lib \
  tests::named_profile_context_window_overrides_conflicting_live_catalog \
  -- --exact --test-threads=1
```

```text
$JCODE_SCRATCH_DIR/lcm-phase56/final-regression-counts-20260724T004314Z
SHA256(SHA256SUMS) = 6996cf38b52aec2a77f8f6bfb716ebe9fb223824a40df83e1e080357a440ff84
```

## Isolated canary, performance, and rollback

Command:

```bash
$JCODE_SCRATCH_DIR/lcm-phase56/run-isolated-canary.sh
```

Runner SHA256: `58595f0674fbebad68c54ab803db37a37a91f1c9cdfe222fc05aa4a2bc6c1121`.

Final-candidate results:

- One diagnostic run overlapped the two-round local `jcode-app-core` campaign and reached the unchanged 15,000 ms timeout. It is retained at `isolated-canary-20260724T032314Z` with manifest digest `54e2d77a...`; it is not counted as a pass.
- After the local suite ended, a threshold file and runner hash were written before a three-run reliability campaign. The fixed rule required all 3/3 runs to pass with no substitution.
- All 24/24 turn responses were exact. Every run published a native LCM graph and recovered `VALUE_ALPHA` from canonical raw history after an explicit rolling restart.
- Preferred route in all runs: `sub2api-codex:gpt-5.6-sol`.
- Preferred completion durations: 8,717 / 8,877 / 8,787 ms, all below 15,000 ms.
- Pre tokens were 14,260 each; post tokens were 461 / 460 / 491.
- `preferred_route_events = 3`; `local_emergency_events = 0`; fallback events = 0.
- The original aggregation executed after all runs but referenced `summary.json` instead of the runner's `canary-summary.json`. The artifact records this setup-only aggregation repair; no run, threshold, or result was replaced.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/canary-reliability-20260724T032500Z
SHA256(SHA256SUMS) = a086d7b9f74541b2b72502c5720165023cc7a077d8b31231be6f9fc657210809
run manifests = b968cbd97b37b4d405c35691e5ec573f9f497a8dbf8c4b2bd8a79676b9a0a33c, 8a094a1f2af50a3473955872d30d7f1f35859b1ee25d295fd09bcd422fb2330a, d1f3831ebe6cb94ccf7f1e5dcc85718b07c8b4e14469ce0e4e4e65b4e05b9135
```

## Rolling versus LCM resource scorecard

The original passing four-session scorecard was provisional because its warm-cache denominator had been selected after inspecting earlier results. It is not promotion evidence. At fixed candidate `01846a4`, the same metric and thresholds were frozen before a fresh four-session run. That run passed every non-cache bound but failed warm-cache delta because one of four LCM warm requests reported zero cache-read tokens: rolling/LCM ratios were 0.9656/0.6961, delta -26.95 percentage points.

A follow-up fixed the sampling granularity rather than changing any threshold: 20 chronological rolling/LCM pairs, alternating which engine ran first, with the identical three-turn source in each pair. With four samples, one 6,912-token miss moves the aggregate by more than 20 percentage points, so 20 pairs allow the pre-existing five-point threshold to be measured rather than quantized to all-or-nothing. The 20-pair preregistration and runner hash were written before execution. The run still failed and no further result is substituted.

| Metric | Threshold | Rolling | LCM |
|---|---:|---:|---:|
| Real compactions | >= 20 LCM | 20 | 20 |
| Aggregate post/pre | <= 0.35 LCM | 0.0092 | 0.0313 |
| Saving-event rate | >= 0.95 LCM | 1.000 | 1.000 |
| Warm cache-read/input | LCM no more than 5 pp below rolling | 0.9656 | 0.7688 |
| Warm requests with cache reads | recorded | 20/20 | 17/20 |
| Durable session bytes | LCM/rolling <= 1.25x | 1,551,453 | 1,726,720 |
| One-message snapshots | 0 | 0 | 0 |
| Preferred/emergency LCM events | all preferred / zero emergency | n/a | 19 / 1 |

Observed LCM minus rolling warm-cache ratio was -19.69 percentage points, outside the -5-point bound. LCM/rolling durable bytes was 1.1130x, post/pre was 0.0313, saving rate was 1.000, all 120 responses were exact, and canonical snapshot+journal message counts were seven for all 40 sessions. Those passing submetrics do not override the cache and emergency failures. `G12 = FAIL`.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/resource-scorecard-20260724T032809Z
SHA256(SHA256SUMS) = 5c0bee780d7b17a7e32bb7a99f0902e2047d11facb78e89f217954a6aa95a2b9

$JCODE_SCRATCH_DIR/lcm-phase56/resource-paired20-20260724T033215Z
SHA256(SHA256SUMS) = 9a67559540811bcfd0900b9b6cff06d101a116d2cc7776084ebb7554eeb52565
runner SHA256 = 676509027da8baa56eb135894eec0aa22ee8c4a0fb6de59ce7d8e97e9617d491
```

## Scheduler evidence

Command:

```bash
JCODE_LCM_SCHEDULER_CERT_OUTPUT="$OUT/scheduler-scorecard.json" \
  cargo test -p jcode-base --lib \
  compaction::tests::lcm_scheduler_fairness_scorecard_has_bounded_p95_and_no_starvation \
  -- --exact --test-threads=1
```

The campaign occupies all three background permits, queues 24 background and 24 critical jobs, proves critical progress through reserved process capacity, then proves both classes drain and all permits return. The 500 ms p95 threshold is enforced by the test.

## Fault, crash, migration, and downgrade evidence

Focused suite commands:

```bash
cargo test -p jcode-base --lib compaction::tests:: -- --test-threads=1
cargo test -p jcode-base --lib session::tests::cases:: -- --test-threads=1
cargo test -p jcode-compaction-core -- --test-threads=1
```

Results: final-candidate compaction 75/75 and compaction-core 18/18; the unchanged session cohort previously passed 67/67. The exact Windows writer-lock test passed 1/1 and the root-library repeated-resume orphan regression passed 1/1. Focused app lifecycle tests for native events, rewind/undo, clone/split, and persisted projection also passed.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/final-local-tests-20260724T034151Z
SHA256(SHA256SUMS) = 8ee551d83bd562d261140646e59eac07b55ea11d19e32c888f7b128ee1af8e47
```

The formal 12-case campaign covers engine switch and downgrade, encrypted-state raw rebuild, durable write failure, atomic parent/leaf publication, stale source rejection, legacy snapshot load, invalid graph fallback, stale writer rejection, torn context transaction, corrupt/glued journal repair, and mutation proof invalidation.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/migration-fault-campaign-20260724T004033Z
SHA256(SHA256SUMS) = d0f9c27bb9d05d124fe25baa6846d5c4d19d053f7b4cd36b1dffb3293b02ace0
```

Process-level command:

```bash
$JCODE_SCRATCH_DIR/lcm-phase56/run-process-kill.sh
```

Runner SHA256: `92e25c554ae43759da609b7729186e4b3937504706c00a1483fc87379bc6541b`.

The campaign issued `SIGKILL` to a PID reported by `fuser` as owning the Unix socket 58 ms after a critical `LCM_SCHEDULER` event. The retained client completed with exit 0 before the later explicit rolling restart. The artifact supports valid snapshot/journal state and subsequent rolling raw-history recovery of `VALUE_CRASH`; it does not prove automatic post-restart recovery of the killed server's in-flight turn or graph event.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/process-kill-20260724T004525Z
SHA256(SHA256SUMS) = 14bf5c2f866540d2411352477311d9169adad14bcc2f5e02d8db69c9094c1222
```

## Independent source security review

An isolated read-only reviewer rejected `a9d3488` because best-effort redaction could send and persist labeled low-entropy or unlabeled mixed-class opaque values. Candidate `60c7bf8` added a conservative LCM-only whole-line classifier and adversarial prompt/output tests. The first re-review then found a distinct deterministic local-emergency bypass through raw file-reference extraction; `9e99e00` routed source, prior summary, and final emergency output through the same classifier and added a graph/persist/reload/materialization regression. The second re-review returned `REMEDIATED`, while identifying historical pre-fix derived nodes as residual re-exposure risk. Final candidate `01846a4` additionally filters restored projection and frontier text at provider materialization; the regression injects historical unsafe derived text and proves it is not exposed.

The classifier deliberately chooses privacy over recall for labeled lines and long mixed-class values. A completely unlabeled natural-language passphrase is intrinsically indistinguishable from ordinary prose and remains an explicit heuristic limitation. Tool payloads are omitted before classification. Rolling remains outside this native-LCM guarantee.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/fixed-candidate-review-20260724.md
SHA256 = bfd347f08fe3dd68d714562b2e965aaee65829b6485fa86227198fa5bef5923a

$JCODE_SCRATCH_DIR/lcm-phase56/secret-rereview-20260724.md
SHA256 = 0d76b062a876ce959178360e97cac50a8280da3bd844ff0636feb4e70be43ef4

$JCODE_SCRATCH_DIR/lcm-phase56/secret-rereview2-20260724.md
SHA256 = 873fa6e4fe77d38e72c6c17aa8e6034e8e125393670c06874a8fdc0e19486bf1
SHA256(review-run SHA256SUMS) = 8c45b4f9028f07dedaf1971dac0ad7a3c09d018a13ff5e3a5e2904bbc4e4c58f
```

## Regression exceptions

The broad fixed-candidate serial run reported twice consecutively from independently clean `HOME` and `JCODE_HOME` directories:

```text
jcode-app-core round 1: 1013 passed, 0 failed, 4 ignored
jcode-app-core round 2: 1013 passed, 0 failed, 4 ignored
```

The command used `cargo test -p jcode-app-core --lib -- --test-threads=1`. Serial execution is required because a subset of tests intentionally mutates process-global environment variables; separate exploratory parallel runs demonstrated that interference and are retained as failed diagnostics, not counted as gate evidence. The exact repeated one-shot resume regression also passed separately.

```text
$JCODE_SCRATCH_DIR/lcm-promotion/app-core-secret-final-serial-20260724T032307Z
SHA256(SHA256SUMS) = 6f247696303aa8d901e45c2b1337636ffc24933b39fd5c44b0b4a08bffdc6446
```

## Windows evidence

Final GitHub Actions workflow dispatch:

```text
repository = tickernelz/jcode
workflow = Windows Smoke
run = 30064063308
URL = https://github.com/tickernelz/jcode/actions/runs/30064063308
head SHA = 01846a47a91aa1a74cd252fc305299c4cf2783f4
conclusion = success
x64 job = 89391412110, windows-latest, success
ARM64 job = 89391412127, windows-11-arm, success
```

The x64 job built the release binary and test executables, then executed eight named native-LCM durability regressions. Each selected exactly one test and passed: writer-lock contention, stale concurrent writer rejection, torn context transaction replay, corrupt-line journal salvage, glued-entry salvage, encrypted-native downgrade/raw rebuild, durable-write fail-closed publication, and stale-source rejection. Two real-binary named-pipe lifecycle tests each selected one test and passed, including server exit/rebind. Binary launch and development-version installer lifecycle passed. The ARM64 hosted job independently built, launched, and installer-verified the ARM64 binary.

The earlier GNU cross-check failure and superseded/cancelled workflow attempts remain historical diagnostics. They are not substituted for run `30064063308`; the archived API metadata asserts both final jobs and the exact head SHA.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/windows-ci-30064063308
SHA256(SHA256SUMS) = b5d9e9949e9b577d002b975c647d2593fea852a32624845b95019ef5bb4ca4c0
```

## Observation window

The fresh campaign ran as transient user-systemd unit
`jcode-lcm-observation-20260724T035333Z.service`, independently of the command
harness's 600-second ceiling. It completed successfully after 1,800 seconds of
paced wall time, with 100/100 exact replies across 10 sessions and 21 canonical
messages per session. Ten compactions were observed, all on exact effective
route `sub2api-codex:gpt-5.6-sol`; emergency and fallback events were zero.
Queue p95 was 0 ms; scheduler starvation, corruption, stale-publication
rejection, and one-message snapshots were all zero.

Earlier harness-bounded observation attempts remain invalid and excluded. The
passing unit restarted from zero, retained the unchanged predeclared thresholds,
was not substituted, and produced its own closed manifest.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/observation-20260724T035333Z
SHA256(SHA256SUMS) = 589f6f4ac9ffc918afc6c6539df7d69dc1985244d33787c13f9c3fc58074df5d
```

## Live-default proof

A redacted parser read only the `compaction.engine` key and recorded no credential data:

```json
{
  "compaction_engine_explicit": "rolling",
  "effective_engine": "rolling",
  "secrets_read": false
}
```

```text
$JCODE_SCRATCH_DIR/lcm-phase56/live-default-final-20260724T035911Z
SHA256(SHA256SUMS) = 0173b36eebf6723549d079566767a6cac294d6b707bdd66538ba48f18d27d13a
```

## Independent evidence-only audit

The historical audit below found and corrected an earlier canary-count overstatement;
it is retained for chronology but is superseded for the final candidate. The final
auditor ran in a new isolated Jcode home and provider session with read-only
instructions. It verified representative manifests, final canary runs, both
app-core rounds, Windows logs/API metadata, final resource campaigns, the closed
observation, rolling live default, and the `SIGKILL` chronology.

The first final review rejected G14 because the ledger overstated the process-kill
artifact as automatic recovery across restart. The ledger now records G08 as
`BLOCKED` and accurately limits that artifact to socket-owner kill handling,
valid durable files, and later rolling raw-history recovery. It also narrows the
artifact-secret statement to configured credentials while acknowledging retained
synthetic secret-shaped fixtures. The final independent correction re-audit then
found the corrected ledger materially evidence-faithful and recommends
`G14 = PASS`. Valid G08 `BLOCKED`, G11 `BLOCKED`, and G12 `FAIL` outcomes remain
unchanged; no rejected review is discarded.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/independent-audit-20260724.md
SHA256 = 8092f000cd718be7129ff1f6002cbeff737215b73f4ba98396c721746331f7b7

$JCODE_SCRATCH_DIR/lcm-phase56/independent-audit-run-20260724T005548Z
SHA256(SHA256SUMS) = e695fa3b888b00441c4329606659f1abd520630572e6a46c8b8a5a8afa982c50

runner SHA256 = 4e927e42c4b3b018454930d8b47aa5d9a80619b765b36d76dc5da3fc4f454a98

$JCODE_SCRATCH_DIR/lcm-phase56/independent-final-audit-20260724.md
SHA256 = 1ef6277c272b7527c84b20e6199102aa19ee2338967b4868f663c379721ec3fa

$JCODE_SCRATCH_DIR/lcm-phase56/independent-final-audit-run-20260724T042652Z
SHA256(SHA256SUMS) = 73b1afaf72f39d636dabc336caf3b57d4f189af0fd3a7665c0b646045f361879

$JCODE_SCRATCH_DIR/lcm-phase56/independent-final-reaudit-20260724.md
SHA256 = 4cb3f0f732a5977b5f44571ed44d9e9531beec58c5dfd04b38b052f2936182af

$JCODE_SCRATCH_DIR/lcm-phase56/independent-final-reaudit-run-20260724T043317Z
SHA256(SHA256SUMS) = 524cce26039ab99457507b72632930693e9f0cd5b5e4bc3ac0aa9c681fedd0ee
```

## Required work before promotion

1. Run a valid process-death/restart campaign that binds the socket owner PID,
   proves the client is interrupted at kill, restarts before recovery/publication,
   and verifies durable replay after restart. Until then G08 remains `BLOCKED`.
2. Provide the exact configured accounts for direct OpenRouter pinning and both
   Gemini auth methods, then run those identities without substitution. Until
   then G11 remains `BLOCKED`.
3. Diagnose and fix the LCM warm-cache regression, including the missing 3 of 20
   LCM cache hits, and eliminate emergency compaction. Rerun a pre-registered
   sustained campaign with the same or stricter bounds. Until then G12 remains
   `FAIL`.
4. Recompute the Boolean-AND only after G08, G11, and G12 have each passed a new
   evidence-bound campaign. Promotion is allowed only if every row is `PASS`.
   Keep `rolling` as the live default and rollback path until a separate
   promotion commit is explicitly authorized.
