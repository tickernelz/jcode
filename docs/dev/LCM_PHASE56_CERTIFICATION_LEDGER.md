# Native LCM Phase 5/6 Certification Ledger

- Certification date: 2026-07-24 UTC
- Branch: `feat/native-lcm-context-engine`
- Native LCM implementation base: `97cb23ba472c80023e0e8eac9e0ab557a3e771f4`
- Certified source revision before this ledger: `f35a7ddfc7931944602f6663d6b1b87cff3b135c`
- External Hermes oracle revision: `main@23d5adc`
- Decision rule: **promote only when every promotion gate is `PASS`**. `FAIL` and `BLOCKED` both evaluate to false.
- Decision: **DO NOT PROMOTE**. Keep `compaction.engine = "rolling"`.

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

## Boolean-AND decision

| ID | Promotion gate | Status | Evidence-based reason |
|---|---|---:|---|
| G01 | Source formatting, focused regressions, binary build | PASS | `cargo fmt --all -- --check`, three exact regressions, native quality/scheduler tests, and `cargo build --bin jcode` passed. |
| G02 | Native 30-trace, two-cycle quality scorecard | PASS | Rolling and native LCM both recalled 150/150 active facts. Wilson 95% lower bound was 0.9750. False completion claims were zero. |
| G03 | External Hermes release, deterministic replay, and shared-corpus comparison | PASS | Official full release validation passed. Deterministic replay completed 60 runs with zero failures and 258/258 canaries. Shared two-cycle corpus passed. |
| G04 | Every configured provider/profile/model route | PASS | Named profile `sub2api-codex` passed credential, provider, and real tool smoke for `gpt-5.6-luna`, `gpt-5.6-sol`, and `gpt-5.6-terra`. |
| G05 | Strict isolated LCM canary on its preferred configured route | **FAIL** | Eight LCM turns and four graph publications completed, but all four compactions used `local:emergency`. Preferred-route compaction timed out at 15 seconds. |
| G06 | Rolling rollback and canonical raw-history recovery | PASS | Both normal rollback and post-SIGKILL downgrade recovered exact planted values from raw history. |
| G07 | Scheduler fairness, starvation, and queue p95 | PASS | 24 background plus 24 critical jobs completed. Starved jobs: 0. Queue p95: 57.323 ms background and 31.342 ms critical, below 500 ms. |
| G08 | Durability, fault, migration, stale-result, mutation, and process-crash campaign | PASS | 12/12 exact migration/fault cases passed. A real socket-owner `SIGKILL` during `LCM_SCHEDULER` automatically recovered the turn and graph event, then rolling recovered raw history. |
| G09 | Full repository regression gate | **FAIL** | Relevant base/session/core suites passed, but broad `jcode-app-core` was 1003 passed, 10 failed, 4 ignored. Two repeat-run restore test collisions were fixed; eight isolated failures outside the focused LCM test set remain. |
| G10 | Windows compile, contention, and crash recovery | **BLOCKED** | GNU cross-check cannot compile without `x86_64-w64-mingw32-gcc`. No real Windows host/runtime was available. Cross-checking on Linux is not runtime proof. |
| G11 | Unconfigured provider/API/account/profile portability | **BLOCKED** | Direct OpenRouter pin/account evidence, Gemini OAuth/API-key evidence, and all other unconfigured routes lack credentials/accounts in this environment. They are not counted as passes. |
| G12 | Sustained resource-growth bounds | **BLOCKED** | Raw token/cache/journal counters were captured, but no predeclared long-window cache and journal-growth acceptance bound was available. Eight one-message reconnect snapshots were observed and must be explained or bounded. |
| G13 | Fixed observation window | **BLOCKED** | Future threshold is fixed at 30 continuous minutes **and** 100 completed turns across at least 10 sessions, with no preferred-route fallback, stale publication, or crash. Available canary evidence covered eight LCM turns in one isolated session and failed the preferred-route condition. |
| G14 | Independent evidence-only audit | PASS | An isolated independent agent recomputed manifests, metrics, route/fault chronology, blockers, secret safety, and the Boolean. Its sole chronology correction was applied verbatim and passed a post-correction re-audit. |
| G15 | Live default and isolation safety | PASS | A redacted check found explicit `compaction.engine = "rolling"`. Canary homes/sockets/configs were isolated. No credential values were copied into artifacts. |

The promotion Boolean is:

```text
PROMOTE = G01 ∧ G02 ∧ G03 ∧ G04 ∧ G05 ∧ G06 ∧ G07 ∧ G08
        ∧ G09 ∧ G10 ∧ G11 ∧ G12 ∧ G13 ∧ G14 ∧ G15
        = false
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

Functional fallback results:

- 8/8 exact turn responses.
- 4 native LCM compaction events.
- Graph generation reached 4 with four durable leaves.
- Rolling rollback recovered `VALUE_ALPHA` from canonical raw history.
- Aggregate observed compaction counters: pre 57,310, post 16,040, saved 41,291; three of four events had positive savings.
- Raw provider counters: nine requests, input 123,729, output 127, cache-read input 55,040, five requests with cache reads. No cross-provider cache hit-rate denominator was invented.
- Durable bytes: 73,904 snapshot, 10,713 journal, 73,670 backup, 169,979 total session files.

Strict-route result:

- `preferred_route_events = 0`.
- `local_emergency_events = 4`.
- Three documented provider-backed critical-route attempts timed out after approximately the fixed 15-second wait (15,016 to 15,037 ms) and fell through to local emergency compaction. All four observed compaction events used `local:emergency`; the artifact does not document a fourth provider-backed timeout attempt.
- Therefore the functional fallback subgate passes, but the promotion canary fails.

An additional growth observation found eight one-message reconnect snapshots. The evidence does not prove whether this is expected bounded reconnect bookkeeping or orphan growth, so sustained growth remains blocked.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/isolated-canary-20260724T003033Z
SHA256(SHA256SUMS) = 091b6324bd380eeadda10fac2f11ce6d8e92a701518a831fa81b58d2c2d2ad61
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

Results: compaction 64/64, session 67/67, compaction core 18/18. Focused app lifecycle tests for native events, rewind/undo, clone/split, and persisted projection also passed.

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

The campaign killed the actual Unix-socket owner with `SIGKILL` 58 ms after a critical `LCM_SCHEDULER` event. Jcode automatically recovered `CRASH_STAGE6_OK` and published one compaction event after restart. An explicit restart on rolling then recovered `VALUE_CRASH` from raw history. Snapshot JSON remained valid and the journal remained replayable.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/process-kill-20260724T004525Z
SHA256(SHA256SUMS) = 14bf5c2f866540d2411352477311d9169adad14bcc2f5e02d8db69c9094c1222
```

## Regression exceptions

A broad serial run reported:

```text
jcode-base compaction: 63 passed
jcode-base session:     67 passed
jcode-compaction-core:  18 passed
jcode-app-core:       1003 passed, 10 failed, 4 ignored
```

The compaction suite became 64/64 after adding the scheduler scorecard. Two restore-session failures were fixed as test-artifact ID collisions, then passed twice consecutively. Eight isolated `jcode-app-core` failures remain in server timing/state and first-party tool-intent schema tests. None is silently waived. Because the full regression gate is Boolean-AND, it remains `FAIL`.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/fault-lifecycle-20260724T003117Z
SHA256(SHA256SUMS) = 00f1926354f477a431a67f015e1c78fa5d4fb717c8839be700b164356f9bec56

$JCODE_SCRATCH_DIR/lcm-phase56/restore-repeatability-20260724T004808Z
SHA256(SHA256SUMS) = f9b41d352750858b5f6c24ed564d3addce95c777305b92e0b0cc0f541b688b7f

$JCODE_SCRATCH_DIR/lcm-phase56/app-core-failure-isolation-20260724T004832Z
SHA256(SHA256SUMS) = b24e8430f06eac03ac7672dc8fb35cbfc24feb8c4bb3303e013360ebf0a68260
```

## Windows evidence

Command:

```bash
cargo check --workspace --target x86_64-pc-windows-gnu
```

The target is installed, but the check fails in `aws-lc-sys` because `x86_64-w64-mingw32-gcc` is unavailable. This is recorded as compile `FAIL`, not pass. No real Windows runtime was configured, so file-lock contention, prepare/persist crash recovery, restart, and downgrade are `BLOCKED`.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/windows-cross-check-20260724T003725Z
SHA256(SHA256SUMS) = 5592e702d59947bae03adbd9f1ac977533e7c44a727146e165afb220228e7033
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
$JCODE_SCRATCH_DIR/lcm-phase56/live-default-status-20260724T005116Z
SHA256(SHA256SUMS) = e4a08a27f662b2e6dd44958868ded966b0e2f9a1e0400451866559b7eadd1e24
```

## Independent evidence-only audit

The auditor ran in a separate isolated Jcode home, used a separate provider session, and had read-only instructions for repository/product files. It independently recomputed representative manifests and raw metrics, checked the strict canary and `SIGKILL` chronology, classified missing routes and Windows evidence, scanned for credential values, and recomputed the Boolean decision.

The first review found one overstatement: the canary documented three provider timeout warnings but four emergency compaction events. The ledger now states those counts separately. The same independent agent re-read the corrected ledger and changed its recommendation to `G14 = PASS`. The correction does not change `G05 = FAIL` or the no-promotion result.

```text
$JCODE_SCRATCH_DIR/lcm-phase56/independent-audit-20260724.md
SHA256 = 8092f000cd718be7129ff1f6002cbeff737215b73f4ba98396c721746331f7b7

$JCODE_SCRATCH_DIR/lcm-phase56/independent-audit-run-20260724T005548Z
SHA256(SHA256SUMS) = e695fa3b888b00441c4329606659f1abd520630572e6a46c8b8a5a8afa982c50

runner SHA256 = 4e927e42c4b3b018454930d8b47aa5d9a80619b765b36d76dc5da3fc4f454a98
```

## Required work before promotion

1. Make the preferred provider-backed critical LCM route finish within the accepted critical path, then rerun the strict isolated canary with zero emergency fallbacks.
2. Resolve all full-suite regressions and rerun the complete suite from a clean test home.
3. Run the exact configured-account matrix for direct OpenRouter pinning, both Gemini auth methods, and every other promotion-scope route. Missing routes remain blocked.
4. Build and run contention, `SIGKILL`, restart, journal recovery, migration, and rolling downgrade on a real Windows host.
5. Predeclare sustained cache and journal-growth limits, explain or eliminate reconnect snapshots, and pass the fixed 30-minute/100-turn/10-session observation window.
6. Recompute the Boolean-AND. Promotion is allowed only if every row is `PASS`.
