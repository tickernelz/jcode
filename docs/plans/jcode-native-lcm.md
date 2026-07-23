# Jcode Native LCM Context Engine: Living Plan

> This document is the implementation source of truth for the approved Jcode-native LCM work.
> Update it at every phase checkpoint with decisions, changed files, validation evidence, unmet gates, and next steps.

## Implementation status

- Initiative: `jcode-native-lcm-context-engine`
- Branch: `feat/native-lcm-context-engine`
- Baseline commit: `6e443c82e456a2e51015e5555a5fcb52f439d410`
- Current phase: **Phase 3/4 hardening, opt-in LCM plus lifecycle and hierarchy**
- Overall status: **in progress**
- Last updated: **2026-07-23**

| Phase | Status | Evidence |
|---|---|---|
| 0. Baseline and correctness prerequisites | Complete | Stable prefix fingerprint; reset/restore/rewind cancel stale work; undo restores exact durable compaction state |
| 1. Minimal control plane | Complete | `rolling|lcm` engine config; route-preserving compaction model; local and authoritative remote `/agents compaction` plus `/agents lcm` |
| 2. Durable graph persistence | Complete | Versioned graph transaction; staged durable commit-before-publication; sequenced/watermarked journal recovery; fault and legacy tests |
| 3. Opt-in depth-zero LCM | In progress | Native Rust leaf generation, adaptive compactor ceiling, typed inherited routes, provider-independent graph, structured output validation, critical fallback chain, and focused safety tests implemented; full provider/fault/quality gates remain |
| 4. Lifecycle and hierarchy | In progress | Immutable fanout-4 hierarchy, recursive same-generation carry, rewind/reload validation, transfer engine snapshot, export stripping, retrieval anchors, and lifecycle guards implemented; full lifecycle and swarm certification remain |
| 5. Canary and validation gates | Blocked | No real provider matrix, external quality comparison, cache/journal benchmark, fault campaign, canary, or rollback evidence yet |
| 6. Default promotion | Blocked | `rolling` remains default; synthetic planted-fact control is not promotion evidence |

## Progress log

### 2026-07-23: implementation authorized

- User approved full autonomous implementation through every phase.
- Repository baseline was clean at `6e443c82e456a2e51015e5555a5fcb52f439d410`.
- No production code had been changed at authorization time.
- Durable initiative and requirement-to-check feedback loop were created.

### 2026-07-23: Phase 0–2 prerequisites completed

- Phase 0 now rejects same-length divergent history when an async summary returns, aborts pending work on reset/restore, invalidates compaction on Agent and local-TUI rewind, and restores the exact durable compaction snapshot on undo.
- Phase 1 adds only `compaction.engine` and optional `compaction.model`. Trigger modes and thresholds are unchanged. Local pickers persist exact route specs; remote pickers mutate authoritative server state, roll back on rejection, and hydrate from the server after reconnect.
- Phase 2 adds immutable versioned context nodes, a small frontier, generation-checked transactions, exact-retry receipts, graph validation, and invalid-derived-state fallback without changing raw-message canonical ownership.
- Session journal entries now carry monotonic sequences. Snapshots install a watermark before best-effort journal retirement, and replay filters covered, duplicate, out-of-order, torn, glued, and stale legacy entries.
- Runtime graph publication now clones and validates a candidate, durably checkpoints it, and only then swaps it into the live session. Write failure leaves live graph state unchanged.
- Remaining filesystem caveat is explicit: Unix durable snapshots fsync the file and attempt parent-directory fsync, but the existing storage helper ignores directory-sync errors; non-Unix replacement retains the existing brief missing-primary limitation.

### 2026-07-23: Phase 3/4 native LCM implementation and hardening

- LCM now uses the canonical raw session journal as source and publishes rebuildable derived graph state through prepare, validate, durable persist, then live publish. Frontier coverage is revalidated on load, recovery, inheritance, imported roots, and candidate commit.
- Graph nodes are immutable and digest-bound. Fanout-4 parent proof preserves ordered children, raw coverage union, chronological antichain frontier, rewind expansion, and reload validation. A new leaf now recursively carries through every full hierarchy suffix in one graph generation, rather than only creating a level-1 parent.
- Imported roots are deterministically bound to recorded source identity, parent transcript hash, and portable summary, but not to the first transfer-child ID, so exact-transcript split descendants can inherit and reload them unchanged. Export strips all derived context graph state so model-written summaries cannot leak through exported sessions.
- Switching from rolling/provider-native to LCM clears opaque or encrypted manager projection and rebuilds from canonical raw history. OpenAI provider-native auto-compaction is suppressed while LCM owns compaction. Rolling and provider-native paths remain available when LCM is not selected.
- Compactor routing now activates inherited routes with typed runtime identity, covering named OpenAI-compatible profiles, OpenAI OAuth/API-key routes, Gemini Code Assist OAuth, and OpenRouter provider preference. Explicit `compaction.model` remains a full route-spec config boundary. The stale-candidate fingerprint includes config/provider profiles, exact session route fields, exact active account labels, and monotonic auth generation. Critical active-route fallback is also bound to the selected-model policy snapshot.
- LCM has one unambiguous nine-section system schema. The legacy rolling instruction is no longer appended. Opaque tool inputs/results are structurally omitted before rolling or LCM model-written compaction, while tool IDs/names and success/error/unknown status remain explicit evidence. Remaining canonical source is secret-redacted before crossing the provider boundary. Every persisted provider-written LCM content line must be an exact contiguous excerpt from that safe source, so invented credentials and semantic-equivalent unsupported completion claims fail closed rather than relying on a bounded phrase list; one rewrite is allowed before failure.
- Critical recovery is bounded and ordered: selected compactor route, active session route, internally consistent first-generation textual legacy summary, then deterministic local emergency compaction. Telemetry records the complete fallback path. Encrypted, iterative, inconsistent, or out-of-range legacy projections are rejected.
- Tool calls/results retain exact call IDs, redacted inputs, result IDs and status, head/tail oversized-result evidence, parallel-call grouping, and consuming assistant response. `conversation_search` can search tool inputs, execute inclusive durable message-ID anchors, and retrieve bounded redacted canonical payload continuations by character offset.
- The process-wide LCM scheduler is capped at four jobs with background admission capped at three, reserving one slot for critical or user-waiting work. Queue wait and provider execution are independently timeout-bounded, cancellation releases both permits, and critical synchronous waits use Tokio `block_in_place` so spawned compactor futures are not starved. Adaptive prompt ceilings are keyed by captured exact route identity.
- Server compaction events expose optional engine, ownership, configured/effective route, fallback path, leaf/parent/frontier counts, maximum level, graph generation, and exact recovery counts. Context-limit auto-recovery now forwards the committed detailed event instead of replacing it with empty synthetic telemetry. The completion bus edge retries briefly until the Tokio task result is visible. Old payload/client shapes remain compatible through optional fields and unknown-field tolerance.
- Local and server transfer paths capture one immutable engine decision and pass it through route selection, artifact generation, and child installation. LCM remains mutually exclusive with rolling/provider-native assembly.
- All session writers now share a per-session cross-process Unix `flock`; graph publication and ordinary saves compare the durable journal sequence and graph generation/operation before writing. Concurrent loaded writers fail closed instead of silently discarding a committed transcript or graph generation, while deliberate rewind generation advances remain valid.
- A deterministic 30-trace planted-fact scorecard exists only as a synthetic regression control. It is not evidence that LCM is better than Hermes or ready to become default.

## Decisions and deviations

Record any approved or evidence-driven deviation here before changing the plan below.

- Phase 4 hierarchy/lifecycle implementation began before Phase 3 received external provider and quality certification because the durability and lifecycle seams had to be exercised together. This does not relax any canary or promotion gate.
- Context-limit classification accepts common provider phrases that omit the word `context` (for example `too many tokens` and `prompt is too long`) while still excluding unrelated failures.

## Verification log

### 2026-07-23: clean detached baseline

- Baseline source: detached worktree at `6e443c82e456a2e51015e5555a5fcb52f439d410`.
- `cargo fmt --all -- --check`: passed.
- Focused command: `cargo test -p jcode-compaction-core -p jcode-base -p jcode-app-core -p jcode-tui`.
- The run reached `jcode-app-core` and started 1,010 tests, then exited 101 with three observed failures before emitting a final count:
  - `server::reload_recovery::tests::garbage_collection_removes_delivered_and_stale_records`, which passed when rerun alone and is classified as baseline cross-test/flaky behavior.
  - `server::swarm_persistence::swarm_persistence_tests::legacy_snapshot_without_mode_defaults_to_light`, which also fails alone at the existing `legacy plan` assertion.
  - `tool::batch::batch_tests::test_schema_only_requires_tool`, which also fails alone because the baseline schema requires `["tool", "intent"]` while the test expects `["tool"]`.
- Artifact: `$JCODE_SCRATCH_DIR/jcode-lcm-baseline-clean.log`.
- These pre-existing failures are not LCM regressions. LCM-focused tests must pass, and final full-suite results will be compared against this baseline rather than reported as wholly green.

### 2026-07-23: Phase 0–2 focused gates

- `cargo fmt --all -- --check`: passed after integration.
- Combined `cargo check` for `jcode-session-types`, `jcode-base`, `jcode-config-types`, `jcode-protocol`, `jcode-app-core`, and `jcode-tui`: passed.
- Session persistence/recovery suite: 63 passed, including durable commit failure, retry after reload, torn transaction, stale sequenced journal, stale legacy journal, and checkpoint healing.
- `jcode-session-types`: 10 passed, including duplicate, missing-child, and cycle graph rejection.
- Focused compaction same-length divergence, Agent rewind/undo, config, protocol, remote-authority, reconnect, and picker tests passed in worker runs and were rerun after integration.
- A combined full-library attempt reached 1,011 `jcode-app-core` tests and reproduced the baseline `tool::batch::batch_tests::test_schema_only_requires_tool` failure. A separate parallel `jcode-base` run exposed five environment/order-sensitive unrelated tests; those paths were untouched and remain outside the LCM focused gate. Final Phase 5 certification must compare fresh isolated runs against the detached baseline.

### 2026-07-23: Phase 3/4 hardening gates

- `cargo fmt --all` and combined `cargo check --tests` passed for the final affected set: `jcode-compaction-core`, `jcode-base`, `jcode-provider-gemini-runtime`, `jcode-app-core`, and `jcode-tui`; the final `--check` and diff gate are repeated immediately before commit.
- All 27 focused `lcm_` tests passed, including adaptive provider ceilings, exact typed route/account policy, opaque tool-payload exclusion, extractive source grounding for arbitrary provider output and completion claims, selected/active/timeout/legacy/local critical fallback, iterative legacy rejection, scheduler capacity/reservation/cancellation/queue timeout, failed/missing/orphan tool evidence, fourth-leaf atomic publication, recursive level-2 carry plus rewind/reload, restart ownership, durability failure, and planted-fact control.
- Full serial compaction suite: 63 passed. The normal parallel compaction run is intentionally not used as certification because test-only `JCODE_HOME`/config sandboxes race process-global config; the serial rerun was green.
- `jcode-session-types`: 10 passed. `jcode-compaction-core`: 18 passed. Session persistence/recovery: 67 passed, including stale concurrent-writer rejection, transferred-root split/reload, and graph-safe covered-message edits/direct transcript truncation. Unix writers use `flock`; the Windows path now uses `LockFileEx`/`UnlockFileEx` and was isolated-target type-checked. Conversation retrieval: 8 passed, including deterministic ordinary-content `response_offset` pagination and explicit next-message anchors after the 50-message cap. Full shared message/redaction suite: 50 passed. Gemini runtime: 30 passed, including typed Code Assist OAuth pinning and fork isolation.
- Server completion-edge test passed. Protocol old-server and old-client compaction compatibility tests passed. OpenAI LCM/provider-native exclusivity test passed.
- Focused TUI results passed: remote compaction telemetry 2, authoritative compaction model/reconnect 4, `/agents lcm` alias 1, rewind/undo 5, local/remote transfer 3, explicit transfer-install snapshot 1, and exact OpenRouter provider-pin persistence 1. Focused Agent rewind and native-compaction coexistence regressions passed. Conversation recovery-telemetry mapping and Gemini typed-route focused tests also passed.
- Entire `jcode-base` serial library run executed 1,162 tests: 1,157 passed and five unrelated existing environment/catalog/config tests failed (`openrouter_like_status_is_provider_specific`, `config_env_fingerprint_tracks_every_apply_env_override_var`, two OpenCode catalog-route tests, and localhost OpenAI-compatible profile configuration). No failed assertion concerns LCM behavior, though `config.rs` is touched only to re-export `CompactionEngine`. These are not counted as green certification and must still be baseline-compared in Phase 5.
- The synthetic 30-trace control recovered 150/150 planted facts with a 0.497 character ratio after full-line extractive grounding. This remains synthetic regression evidence only.
- Independent read-only audits found concrete graph-CAS, route/auth, scheduler, stale-fallback, restart, prompt-secret, output-grounding, imported-split, retrieval-continuation, transfer-snapshot, recovery-telemetry, cross-platform writer-lock, and canonical-transcript mutation defects. Those findings were fixed and covered by focused regressions. The targeted post-fix re-audit found no remaining concrete correctness/security blocker in its four final areas.
- **Promotion remains blocked:** no objective real-provider route/account matrix, external Hermes-versus-LCM quality corpus, real token/latency/cache-hit/journal-amplification benchmark, comprehensive fault injection, scheduler fairness/queue-p95 evidence, real Windows compile/runtime contention run, multi-hop/multibyte retrieval campaign, canary deployment, rollback drill, or production observation window exists yet.

---

## Approved implementation plan

```plan
# Rencana Revisi: Jcode Native LCM Context Engine

## 1. Keputusan arsitektur

1. **LCM adalah context engine**, bukan `CompactionMode`.
   - `reactive | proactive | semantic` tetap menentukan kapan dan di mana compaction dilakukan.
   - `rolling | lcm` menentukan bagaimana context hasil compaction disimpan dan dirakit.
   - Jangan menambahkan `CompactionMode::Lcm`.

2. **LCM dibuat native Rust di Jcode.**
   - Tidak mengimpor plugin Python Hermes.
   - Tidak menambahkan SQLite atau database raw-message kedua.
   - Tidak menambahkan runtime atau tool dependency pada Hermes.
   - Hermes hanya dipakai sebagai referensi desain dan pembanding benchmark eksternal.

3. **Session snapshot dan journal Jcode tetap menjadi source of truth.**
   - Raw `StoredMessage` tidak dihapus oleh LCM.
   - Summary graph adalah derived state yang dapat divalidasi, diabaikan, atau dibangun ulang.
   - Durable memory tetap subsystem terpisah. Jangan menyimpan graph LCM ke memory store.

4. **Compactor lama dipertahankan sebagai rollback dan emergency fallback.**
   - Tidak boleh ada dua compactor yang memegang ownership pada saat yang sama.
   - Provider-native, rolling, dan LCM harus memiliki policy ownership yang eksplisit.

---

## 2. Konfigurasi minimal

Tambahkan hanya dua field:

```toml
[compaction]
engine = "rolling"          # rolling | lcm
# model = "openai-api:gpt-5.5"
```

### Semantik

- `compaction.engine`
  - Default awal: `rolling`, sehingga upgrade tidak mengubah perilaku existing session.
  - Setelah semua release gate lulus, default dapat diubah menjadi `lcm` pada commit dan release terpisah.
  - `rolling` tetap tersedia sebagai rollback.

- `compaction.model`
  - Optional.
  - Dipilih melalui `/agents compaction`.
  - `/agents lcm` menjadi alias.
  - Mengubah model **tidak pernah** mengaktifkan engine LCM.
  - Jika unset, compactor mewarisi provider fork dan route aktif session secara lengkap, bukan hanya nama model.

### Yang tidak ditambahkan

Tidak ada config baru untuk:

- Threshold LCM.
- Fresh-tail size.
- Level atau kedalaman graph.
- Condensation fanout.
- Search limit.
- Chunk size.
- Retry count.
- Semaphore size.
- Embedding backend.
- Node retention.

Semua itu diturunkan dari context window, token accounting, dan invariant internal yang diuji.

### Threshold yang dipertahankan persis

Gunakan policy Jcode yang sekarang:

- Standard trigger: `COMPACTION_THRESHOLD = 0.80`.
- Critical recovery: `CRITICAL_THRESHOLD = 0.95`.
- Fresh tail: `RECENT_TURNS_TO_KEEP = 10`.
- Manual minimum dan current hard-compaction policy.
- Semua `CompactionConfig` proactive dan semantic:
  - `lookahead_turns`
  - `ewma_alpha`
  - `proactive_floor`
  - `min_samples`
  - `stall_window`
  - `min_turns_between_compactions`
  - `topic_shift_threshold`
  - `relevance_keep_threshold`
  - `goal_window_turns`

Tidak mengadopsi threshold atau fresh-tail default Hermes.

---

## 3. `/agents` sebagai model control plane

### UX

Tambahkan target baru:

- Label: `LCM compactor`
- Command utama: `/agents compaction`
- Alias: `/agents lcm`
- Config path: `compaction.model`
- Inherit row: route aktif session saat compaction dieksekusi.

Perubahan utama berada di:

- `crates/jcode-config-types/src/lib.rs`
- `crates/jcode-tui/src/tui/mod.rs`
- `crates/jcode-tui/src/tui/app/commands.rs`
- `crates/jcode-tui/src/tui/app/state_ui_input_helpers.rs`
- `crates/jcode-tui/src/tui/app/input_help.rs`
- `crates/jcode-tui/src/tui/app/inline_interactive/helpers.rs`
- `crates/jcode-tui/src/tui/app/inline_interactive/openers.rs`
- Test picker dan command terkait.

Ganti hardcoded `(0..5)` pada picker dengan panjang collection aktual.

### Full route identity

`compaction.model` boleh tetap berupa route spec yang mudah diedit, tetapi runtime harus mengubahnya menjadi typed route selection.

Route identity harus mempertahankan:

- Model.
- Provider atau named provider profile.
- API method.
- Endpoint.
- OAuth versus API-key route.
- Account atau provider fork yang relevan.

Model dengan nama sama pada dua route berbeda tidak boleh dipilih secara ambigu.

Jika model unset:

1. Server mengambil provider fork session aktif.
2. Model dan route session aktif dipertahankan.
3. Tidak menulis hasil inheritance tersebut menjadi explicit config.
4. Perubahan model utama session otomatis ikut pada compaction berikutnya.

### Local dan remote authority

Saat TUI remote:

1. Picker mengirim request mutation ke server.
2. Server memvalidasi dan menyimpan config.
3. Server mengirim authoritative result kepada seluruh attached client.
4. UI tidak mempertahankan optimistic state jika server menolak.
5. Reconnect membaca state server, bukan config lokal client.

Scope V1 adalah **global server default**, sesuai perilaku saved role model pada `/agents`. Perubahan berlaku pada compaction job berikutnya. Tidak ada per-session model override baru.

Job yang sedang berjalan menangkap policy generation. Jika engine atau model diubah, hasil job lama dibatalkan atau ditolak sebelum commit.

---

## 4. Satu ownership path untuk local TUI dan server Agent

Jcode sekarang memiliki dua caller compaction:

- Local TUI di `jcode-tui/src/tui/app/conversation_state.rs`.
- Server Agent di `jcode-app-core/src/agent.rs` dan `agent/compaction.rs`.

Keduanya harus memakai satu facade di `jcode-base`.

Pertahankan `CompactionManager` sebagai public facade agar downstream churn kecil, lalu pisahkan implementasi internal:

```text
CompactionManager
├── trigger policy
├── rolling state
├── LCM graph state
├── prepared job
└── context materialization
```

Tambahkan modul internal seperti:

- `crates/jcode-base/src/compaction/graph.rs`
- `crates/jcode-base/src/compaction/prompt.rs`
- `crates/jcode-base/src/compaction/assembly.rs`

Local TUI dan Agent hanya bertanggung jawab untuk:

1. Menyediakan canonical session messages.
2. Memulai atau mem-poll prepared job.
3. Menjalankan shared durable commit.
4. Mereset cache dan provider session satu kali setelah publication.
5. Menampilkan event.

Trigger evaluation, source validation, frontier construction, dan provider-facing materialization tidak boleh diduplikasi.

---

## 5. Compaction ownership matrix

| Engine | Provider native auto | Owner aktif |
|---|---:|---|
| `rolling` | aktif | Existing provider-native path |
| `rolling` | tidak aktif | Existing Jcode rolling compactor |
| `lcm` | kondisi apa pun | Jcode LCM |

Saat `engine = "lcm"`:

- Provider-native auto compaction harus disuppress untuk session tersebut.
- `provider.native_compact()` tidak boleh dipanggil oleh artifact generator.
- Encrypted OpenAI compaction lama tidak boleh diperlakukan sebagai summary node.
- Native state existing tetap aktif sampai LCM berhasil merangkum ulang canonical raw prefix.
- Handoff ke LCM dilakukan atomically. Jika konversi gagal, native state lama tetap aktif.

Dengan demikian tidak ada dua mechanism yang dapat memajukan frontier secara bersamaan.

---

## 6. Persisted graph yang append-only dan journal-native

### Persisted types

Tambahkan versioned types di `jcode-session-types`:

```text
StoredContextNode
- id
- schema_version
- level
- source_session_id
- ordered source_message_ids untuk leaf
- source_sha256
- child_node_ids untuk parent
- summary_text
- estimated_tokens
- summarizer model/provider/route
- prompt_schema_version
- created_at

StoredContextFrontier
- schema_version
- generation
- active_node_ids
- covered_message_count
- covered_through_message_id
- source_prefix_sha256
- next_node_sequence
```

Leaf menyimpan ordered canonical message IDs satu kali. Parent menyimpan child IDs dan aggregated source proof, bukan mengulang seluruh message-ID list.

Gunakan SHA-256 yang sudah tersedia di workspace. Jangan memakai `DefaultHasher` untuk persisted fingerprint karena stabilitasnya tidak dijamin lintas build.

### Session layout

Tambahkan:

- `Session.context_nodes: Vec<StoredContextNode>`
- `Session.context_frontier: Option<StoredContextFrontier>`

Pertahankan `Session.compaction` untuk legacy rolling dan provider-native migration.

### Journal transaction

`append_context_nodes` adalah desain yang benar, dengan syarat graph penuh tidak ditulis ulang pada setiap journal meta.

Gunakan transaction-shaped record:

```text
ContextGraphTransaction
- op_id
- base_generation
- generation
- append_context_nodes
- frontier
- input_proof
```

Transaction tersebut berada pada satu `SessionJournalEntry` bersama delta session terkait.

Invariant:

- Frontier hanya boleh mereferensikan node yang sudah persisted sebelumnya atau berada pada transaction yang sama.
- Duplicate `op_id` bersifat idempotent.
- Duplicate node ID hanya diterima jika canonical serialized payload identik.
- Generation harus bertambah tepat satu dari validated base generation.
- Tidak ada delete atau replacement melalui `append_context_nodes`.
- Rewind dan repair hanya mengganti active frontier. Immutable nodes tetap disimpan sampai GC yang terpisah terbukti aman.

### Menghindari journal amplification

- Snapshot menyimpan graph penuh.
- Journal hanya menyimpan node baru.
- Journal meta hanya membawa frontier kecil.
- Graph tidak diserialisasi ulang pada setiap message append.
- Journal size tetap linear terhadap node baru, bukan jumlah node dikalikan jumlah turn.

---

## 7. Durable prepare, persist, publish

Background task tidak boleh memutasi live manager atau session state.

### Protocol

1. **Capture**
   - Tangkap engine dan policy generation.
   - Tangkap graph generation.
   - Tangkap ordered source message IDs.
   - Hitung canonical SHA-256.
   - Tangkap exact compactor route.

2. **Prepare**
   - Lakukan summary dan condensation di background.
   - Hasilnya berupa immutable `PreparedContextTransaction`.
   - Tidak ada active frontier yang berubah.

3. **Validate**
   - Cocokkan kembali engine dan policy generation.
   - Cocokkan graph base generation.
   - Verifikasi message IDs, ordering, source-session identity, dan content hash.
   - Append-only tail baru diperbolehkan selama captured prefix tetap identik.
   - Equal-length divergent branch harus ditolak.
   - Verifikasi tool transaction boundary.
   - Verifikasi graph acyclic dan seluruh child tersedia.

4. **Persist**
   - Stage delta tanpa mengubah visible manager state.
   - Simpan transaction dan frontier melalui strict durable session save.
   - Durable save adalah commit point.

5. **Publish**
   - Setelah storage menyatakan transaction durable, publish frontier ke manager.
   - Publication dibuat infallible dan dapat direkonstruksi dari session state.
   - Reset KV-cache generation dan provider session tepat satu kali.

6. **Failure**
   - Save gagal berarti live manager dan in-memory session tetap pada frontier lama.
   - Prepared transaction dapat dibatalkan atau di-retry secara idempotent.
   - Tidak ada partial summary yang menjadi provider-visible.

### Crash semantics

Harus benar pada setiap window:

- Sebelum summary selesai.
- Setelah summary selesai tetapi sebelum validation.
- Saat journal append.
- Setelah journal durable tetapi sebelum manager publication.
- Saat snapshot checkpoint.
- Setelah snapshot install tetapi sebelum journal retirement.
- Saat cache reset.
- Saat fallback dimulai.

Jika crash terjadi setelah durable save tetapi sebelum publication, restore harus memuat frontier committed dan melanjutkan tanpa membutuhkan state dari manager lama.

### Snapshot dan journal watermark

Perkuat session persistence:

- Journal entry memiliki monotonic sequence atau transaction watermark.
- Snapshot menyimpan journal watermark terakhir yang sudah tercakup.
- Replay mengabaikan entry pada atau sebelum watermark.
- Snapshot ditulis ke temporary file, di-flush, di-fsync, di-rename atomically, lalu parent directory di-fsync.
- Journal lama baru dipensiunkan setelah snapshot installed.
- Jika journal deletion gagal, watermark mencegah duplicate replay.
- Torn final record diabaikan dan dilaporkan.
- Context transaction append memakai full-write handling dan durable flush.
- Per-session writer diserialisasi. Generation CAS tetap menjadi defense terhadap stale multi-writer result.

---

## 8. LCM V1: depth-zero yang benar sebelum hierarchy

Hierarchy tidak langsung dibuat sebelum basic path lolos benchmark dan fault tests.

### Source selection

- Gunakan existing reactive, proactive, atau semantic cutoff.
- Gunakan existing `safe_compaction_cutoff()`.
- Jangan memisahkan assistant tool call dari seluruh corresponding tool result.
- Fresh tail minimal 10 messages.
- Jika batas 10 memotong tool transaction, perluas tail ke belakang sampai transaction lengkap.

Definisikan tool transaction mencakup:

- Assistant tool-use.
- Semua corresponding results, termasuk parallel results.
- Retry atau repair marker.
- Assistant response yang mengonsumsi hasil bila sudah tersedia.

Untuk tool result yang secara individual melebihi context compactor:

- Jangan memecah ordering transaction.
- Gunakan bounded head/tail projection dan durable retrieval marker yang membawa message ID/hash.
- Raw payload tetap ada di session journal dan dapat diambil melalui `conversation_search`.
- Jangan menambahkan tool-output externalization subsystem pada V1.

### Compactor-aware chunking

Jangan mengirim seluruh source prefix ke model compaction tanpa memperhatikan context model yang dipilih.

Hitung input budget dari:

- Resolved compactor context window.
- Summary system prompt.
- Output-token reserve.
- Provider safety margin.
- Token-estimation error reserve.
- Existing frontier yang perlu disertakan.
- Tool-transaction boundaries.

Jika route metadata tidak akurat atau provider mengembalikan context-limit error:

1. Batalkan attempt tersebut.
2. Kecilkan chunk secara deterministik.
3. Retry dengan adaptive split.
4. Simpan observed safe ceiling dalam runtime cache beserta route identity dan provenance.
5. Jangan menjadikannya config publik.

### Coding-specific summary schema

Setiap node menghasilkan Markdown stabil dengan bagian:

1. Objective dan user intent.
2. Explicit constraints dan prohibited actions.
3. Decisions beserta rationale.
4. Repository state, paths, symbols, branches, dan commits.
5. Changes yang benar-benar dilakukan.
6. Commands dan tests beserta hasil aktual.
7. Failures, diagnosis, dan unresolved blockers.
8. Open questions dan next steps.
9. Retrieval anchors berupa keywords dan source message range.

Prompt wajib:

- Membedakan fakta terobservasi dari rencana atau asumsi.
- Tidak mengklaim test atau edit telah dilakukan jika belum.
- Mempertahankan koreksi yang lebih baru.
- Menandai keputusan yang superseded.
- Tidak menyimpan chain-of-thought.
- Tidak menyalin credentials, token, atau raw tool blob.
- Tidak mengarang detail yang tidak ada pada source.

Validasi output:

- Tidak kosong.
- Benar-benar lebih kecil daripada source projection.
- Berada dalam summary budget.
- Tidak kehilangan mandatory critical facts pada fixture tests.
- Oversized output menjalani concise rewrite satu kali.
- Jika tetap gagal, gunakan legacy fallback atau hard recovery, bukan publish output buruk.

---

## 9. Hierarchical condensation setelah V1 lulus

Setelah depth-zero path stabil:

- Frontier berisi immutable chronological nodes.
- Hanya adjacent nodes yang boleh digabung pada V1 hierarchy.
- Tidak ada semantic reordering karena akan merusak auditability.
- Parent mencakup contiguous source span.
- Parent prompt merekonsiliasi contradiction berdasarkan chronology.
- Correction lineage tetap dipertahankan.

Condensation dijalankan hanya jika:

```text
estimated(frontier + fresh tail) >= existing current compaction budget
```

Tidak ada threshold LCM baru.

Sebelum background work dimulai:

1. Hitung seluruh condensation plan yang diperlukan.
2. Batasi jumlah pekerjaan agar tidak menimbulkan latency cliff.
3. Generate leaf dan parent yang dibutuhkan.
4. Commit semuanya sebagai satu graph generation.
5. Lakukan satu provider-visible prefix change dan satu cache-generation bump.

Jangan publish leaf terlebih dahulu lalu parent pada turn berikutnya jika keduanya sudah dibutuhkan untuk fit.

---

## 10. Provider-aware context assembly

Provider-facing context terdiri dari:

1. Stable synthetic compacted-context block.
2. Active frontier nodes dalam chronological order.
3. Fresh raw tail tanpa perubahan.
4. Existing transient reminders pada posisi yang sudah ditentukan Jcode.

Properties:

- Byte representation node yang sudah committed tidak berubah.
- Cache marker memakai helper provider Jcode yang existing.
- Tidak ada volatile recall yang disisipkan di tengah stable summary prefix.
- Frontier hanya berubah ketika satu compaction generation dipublish.
- Model switch melakukan retokenization dan fit check tanpa mengubah persisted summaries.
- Provider session dan KV-cache direset sekali setelah publication, tidak saat background generation.

---

## 11. Retrieval melalui `conversation_search`

Tidak menambahkan tool zoo Hermes seperti grep, recent, recall, describe, inspect, dan doctor.

Perbaiki existing `conversation_search`:

- Tetap membaca canonical raw session journal.
- Lepaskan compaction-manager read lock sebelum disk load dan search.
- Pertahankan maksimum 10 search matches.
- Tambahkan hard cap untuk turn-range dan total output characters/tokens.
- Return message IDs, source session, dan canonical turn range.
- Statistik mencakup engine, frontier size, node level, covered count, dan fresh tail.
- Summary node memberi instruksi dan retrieval anchors agar model tahu kapan memanggil `conversation_search`.
- Search tidak boleh bergantung pada graph yang sehat.
- Tidak boleh ada cross-session retrieval tanpa lineage dan authorization yang eksplisit.

Untuk transfer child, ancestor lookup hanya diperbolehkan sepanjang recorded parent chain. Hasil harus menyebut source session dengan jelas.

Tidak menambahkan `context_expand` pada V1 karena raw turn-range retrieval sudah memenuhi kebutuhan yang sama dengan sumber lebih authoritative.

---

## 12. Lifecycle semantics

### Append biasa

- Graph tetap valid.
- Background result boleh dipublish jika captured prefix tidak berubah.

### Rewind

- Cancel atau invalidate pending generation.
- Naikkan branch/policy generation.
- Pertahankan maximal contiguous frontier prefix yang seluruh source messages-nya masih ada dan hash-nya valid.
- Drop frontier node yang overlap dengan removed region dan semua frontier node sesudahnya.
- Tidak boleh ada informasi dari turn yang telah di-rewind pada provider context.

### Undo rewind

`RewindUndoSnapshot` harus menyimpan:

- Messages.
- Graph frontier atau committed graph generation.
- Relevant node reachability.
- Provider session IDs.
- Cache generation metadata yang diperlukan.

Undo mengembalikan exact prior committed frontier, bukan sekadar message vector.

### Message mutation

Untuk insert, replace, truncation, image stripping, atau tool-result repair:

- Cari leaf pertama yang ID/hash-nya berubah.
- Retain only valid prefix.
- Invalidate suffix.
- Cancel stale background results.
- Rebuild dari raw messages bila diperlukan.

### Split dan fork

- Raw messages yang disalin membawa reachable immutable nodes dan frontier.
- Node namespace menyimpan original source-session identity.
- Child tail dimulai setelah fork notice.
- Tidak ada reference ke source range yang tidak ikut disalin.

### Transfer

- Buat satu self-contained imported root pada child.
- Root menyimpan parent-session lineage dan source proof.
- Root tidak memiliki dangling child-node references.
- Raw parent tetap canonical dan dapat dicari hanya melalui authorized ancestor lineage.
- Import summary, lineage, dan frontier dilakukan atomically.

### Swarm

- Fresh swarm worker mendapat graph namespace kosong.
- Tidak ada coordinator history atau frontier yang bocor otomatis.
- Setiap session hanya boleh memiliki satu compaction job aktif.
- Gunakan cancellation-safe bounded scheduler dengan queue-time telemetry.
- Emergency recovery tidak boleh mati menunggu permit yang sedang dipakai failed background job.
- Mulai dengan process-wide safety ceiling tanpa user config. Pindah ke per-provider scheduler hanya jika benchmark menunjukkan head-of-line blocking.

### Export dan redaction

- Karena graph derived dan menduplikasi fakta raw transcript, export dapat menghilangkan graph sepenuhnya.
- Jika graph disertakan untuk debug, seluruh node harus melewati redaction yang sama dengan session export.
- Summary text tidak boleh masuk log atau telemetry.

---

## 13. Fallback dan recovery

### Normal background failure

Jika explicit `compaction.model` gagal sebelum critical threshold:

- Jangan diam-diam memakai model lain.
- Batalkan job.
- Pertahankan frontier lama.
- Tampilkan reason dan effective route.
- Retry pada kesempatan berikutnya sesuai cooldown existing.

### Critical recovery

Pada atau di atas 95%:

1. Coba selected compactor route dengan bounded attempt.
2. Jika gagal, gunakan active session provider route.
3. Jika LCM generation masih tidak dapat dibuat, gunakan legacy rolling summary sebagai imported root melalui durable commit path yang sama.
4. Terakhir gunakan existing hard compaction atau emergency truncation.
5. Raw journal tetap dipertahankan.
6. Setiap transition mencatat structured reason code.

Output dari timed-out atau superseded attempt tidak boleh dipublish setelah fallback berhasil.

### Invalid persisted graph

- Validasi graph saat restore.
- Jika node hilang, cycle ditemukan, hash salah, atau frontier menunjuk generation invalid:
  - Jangan panik.
  - Abaikan invalid frontier suffix.
  - Gunakan maximal valid prefix atau legacy state.
  - Jika perlu, gunakan raw context dan hard recovery.
  - Schedule rebuild melalui transaction/CAS normal.
  - Emit structured diagnostics tanpa summary content.

---

## 14. Observability

Perluas existing `CompactionEvent` dan debug output dengan:

- Trigger mode.
- Engine.
- Ownership path: native, rolling, atau LCM.
- Configured dan effective compactor route.
- Fallback reason.
- Source message dan token count.
- Leaf dan parent count.
- Frontier size dan maximum level.
- Graph generation.
- Pre/post tokens.
- Compression ratio.
- Summary latency.
- Queue latency.
- Persistence latency.
- Stale-result rejection.
- Graph validation atau rebuild count.
- Cache-generation bump count.
- Journal dan graph bytes.

Extend existing `/compact` status atau context status output. Jangan menambah slash command baru hanya untuk graph.

---

## 15. Test strategy

### Config dan `/agents`

- Missing engine default ke `rolling`.
- Parse dan round-trip `rolling | lcm`.
- Unknown value gagal dengan jelas.
- Mode, engine, dan model independen.
- Memilih model tidak mengaktifkan LCM.
- Clear model mengembalikan exact-route inheritance.
- Duplicate model names pada provider berbeda memilih route yang tepat.
- Remote server mutation, rejection rollback, broadcast, reconnect, dan restart.
- Active job dengan policy yang berubah ditolak sebelum commit.

### Graph invariants

- Node IDs unique.
- Duplicate ID hanya idempotent jika payload identik.
- Parent tersedia.
- Graph acyclic.
- Frontier chronological dan non-overlapping.
- Coverage contiguous.
- Source IDs dan SHA-256 cocok.
- Generation bertambah tepat satu.
- Identical input menghasilkan identical boundaries dan lineage.

### Persistence dan fault injection

Inject failure:

- Sebelum dan sesudah summary.
- Sebelum journal append.
- Partial JSONL write.
- Setelah append sebelum fsync.
- Setelah durable append sebelum publication.
- Saat snapshot temp write.
- Setelah snapshot rename sebelum journal cleanup.
- Saat duplicate retry.
- Saat journal contains corrupt atau glued lines.
- Saat disk full dan permission failure.

Expected result selalu salah satu dari:

- Frontier lama valid.
- Frontier baru lengkap dan valid.

Tidak boleh ada intermediate state.

### Tool transactions

- Single tool call.
- Parallel calls.
- Missing-result repair.
- Retry.
- Large result.
- Image payload.
- Tool result tepat di cutoff.
- Oversized transaction melebihi compactor window.

Semua call dan result tetap paired dan ordered.

### Lifecycle

- Resume setelah satu dan banyak compaction.
- Rewind di fresh tail.
- Rewind ke dalam summarized prefix.
- Undo rewind.
- Equal-length divergent branch.
- Split, fork, selfdev, ambient, overnight, transfer.
- Clear session.
- Provider dan model switch.
- Native encrypted-state migration.
- Old un-compacted dan rolling sessions.
- Old client atau binary yang mengabaikan unknown graph fields.
- Maximum configured swarm concurrency dan session isolation.

### Local/server parity

Fixture yang sama melalui local TUI dan server Agent harus menghasilkan:

- Provider-message hashes yang sama.
- Trigger dan cutoff yang sama.
- Frontier yang sama.
- Persisted graph yang sama.
- Event yang sama.

---

## 16. Benchmark untuk membuktikan lebih baik dari Hermes

Buat benchmark harness Rust dengan tiga baseline:

1. Current Jcode rolling/native.
2. Jcode LCM.
3. Hermes LCM sebagai external black-box benchmark oracle saja.

Gunakan minimal 30 scrubbed long coding traces, termasuk:

- Multi-file implementation.
- User constraint corrections.
- Repeated test-fix cycles.
- Tool-heavy turns.
- Topic shifts.
- Long error logs.
- Model/provider switch.
- Compactor model dengan context lebih kecil.
- Rewind dan divergent branch.
- Crash dan timeout injection.
- Swarm concurrency.
- Planted decisions, file references, errors, dan superseded facts.

Gunakan compactor model dan evaluator yang sama sejauh memungkinkan. Laporkan median, p95, confidence interval, serta per-fixture critical failures.

### Mandatory gates sebelum default

- Continuity dan next-action accuracy tidak lebih buruk dari baseline terbaik lebih dari 2 percentage points.
- Zero invented completed edits, commits, atau tests pada critical fixtures.
- Minimal 95% exact recovery untuk planted decisions, paths, errors, corrections, dan constraints melalui active context plus bounded search.
- 100% tool-call/result integrity.
- Zero cross-session leakage.
- Zero partial frontier pada seluruh crash/cancellation tests.
- Zero uncaught normal context overflow.
- 100% bounded recovery untuk forced critical dan oversized-transaction cases.
- No-compaction p95 overhead di bawah 5% atau 100 ms, mana yang lebih besar.
- Compaction p95 tidak lebih lambat dari current Jcode lebih dari 20%.
- Cache-read ratio tidak turun lebih dari 5 percentage points terhadap rolling.
- Total compactor tokens tidak lebih dari 1.25 kali current Jcode kecuali quality naik minimal 5 points.
- Median provider-facing tokens minimal 10% lebih kecil dari Hermes untuk trace multi-cycle.
- Journal write amplification minimal 50% lebih rendah daripada persistence graph Hermes pada workload yang sama.
- Journal dan node growth terbukti linear.
- Semaphore queue p95 di bawah satu normal compaction duration.

LCM baru dapat disebut lebih baik dan dijadikan default jika:

- Tidak ada regression pada durability, critical recall, cache safety, dan lifecycle correctness.
- Ada peningkatan terukur pada sedikitnya dua area: continuity, prompt tokens, journal amplification, restore reliability, atau compaction cost.

---

## 17. Fitur yang sengaja ditunda

Jangan bangun pada iterasi awal:

- Per-node embeddings.
- Vector database.
- Semantic DAG reordering.
- Temporal atau cross-session memory graph.
- Entity extraction.
- Automatic tool-output externalization baru.
- Hermes tool suite.
- Graph visualization UI.
- User-editable summaries.
- Adaptive trigger thresholds.
- Per-provider scheduler kompleks.
- Graph garbage collection.
- Background speculative condensation.
- Separate LCM database atau daemon.

Fitur tersebut hanya dipertimbangkan jika benchmark menunjukkan exact raw search, chronological graph, atau storage linear belum cukup.

---

## 18. Urutan implementasi dan commit

### Phase 0: baseline dan correctness prerequisites

- Tambahkan benchmark fixtures current Jcode dan Hermes.
- Formalisasi ownership state.
- Satukan local/server context-engine facade.
- Perbaiki existing rewind dan stale-result correctness.
- Commit terpisah.

### Phase 1: minimal control plane

- Tambahkan `compaction.engine` dan `compaction.model`.
- Tambahkan `/agents compaction` dan alias `/agents lcm`.
- Implement typed route resolution.
- Implement remote authoritative config mutation.
- Commit dan jalankan focused config/TUI/protocol tests.

### Phase 2: durable persistence substrate

- Tambahkan node/frontier types.
- Tambahkan append-only context transaction.
- Tambahkan generation CAS, op ID, hash validation, watermark, dan durable checkpoint semantics.
- Tambahkan replay/recovery fault tests.
- Belum mengubah provider-facing context.
- Commit terpisah.

### Phase 3: opt-in depth-zero LCM

- Implement compactor-aware chunking.
- Implement coding summary schema.
- Implement prepare, persist, publish.
- Implement immutable leaf frontier dan fresh tail.
- Implement exclusive provider ownership dan fallback.
- Implement raw bounded `conversation_search`.
- Default tetap `rolling`.
- Commit dan jalankan local/server/provider matrix.

### Phase 4: lifecycle dan hierarchy

- Implement rewind/undo reconciliation.
- Implement split, fork, transfer, swarm isolation.
- Implement adjacent hierarchical condensation.
- Implement one-generation atomic leaf-plus-parent publication.
- Implement cache-aware assembly.
- Commit setelah lifecycle dan cache tests lulus.

### Phase 5: canary

- Aktifkan `engine = "lcm"` hanya pada canary sessions/builds.
- Jalankan benchmark, crash matrix, provider matrix, dan long-running real traces.
- Pertahankan automatic fallback dan rollback ke `rolling`.
- Tidak ada default switch jika salah satu mandatory gate gagal.

### Phase 6: default promotion

- Ubah default ke `lcm` dalam release terpisah.
- Pertahankan `rolling` dan provider-native path sebagai fallback minimal satu release cycle.
- Pantau fallback rate, graph repair, context overflow, cache hit, queue latency, dan journal growth.
- Hapus legacy path hanya melalui proposal dan migration plan terpisah.

---

## 19. Hasil yang dituju

Dibanding Hermes, implementasi ini menjadi:

- **Lebih optimal:** native Rust, tanpa Python/SQLite roundtrip, append-only node deltas, chunking mengikuti context model yang nyata, dan cache-stable immutable frontier.
- **Lebih smart:** coding-specific summaries, correction lineage, exact tool-transaction boundaries, adaptive chunk splitting, provider-aware assembly, serta direct raw recall.
- **Lebih durable:** raw journal canonical, source IDs plus SHA-256, generation CAS, strict persist-before-publish, replay idempotent, snapshot watermark, dan rebuild tanpa kehilangan transcript.
- **Lebih portable:** compactor model dipilih independen melalui `/agents`, route identity dipertahankan, dan graph text tidak terikat provider.
- **Lebih sederhana dioperasikan:** hanya dua config baru, threshold lama tetap berlaku, tidak ada database atau tool suite baru.

Tidak ada implementasi, todo, config, atau git state yang diubah sebelum plan ini disetujui.
```
**Planning durable storage with fsync**

**Designing durable snapshot and journal sequencing**

**Finalizing per-session locking and durability**

**Designing internal scheduler with global ceiling**

**Planning bounded scheduler and tool result compaction**

**Implementing context reserve and error handling**

**Planning condensation and compaction strategy**

**Clarifying compaction config scope and plan formatting**

**Drafting Indonesian LCM revision outline**

**Planning durable graph persistence phases**

**Defining compaction and commit architecture**

**Detailing graph node data structures**

**Deciding self-contained imported root transfer**

**Evaluating performance criteria against Hermes**

**Defining Pareto improvement criteria and approval process**

**Refining punctuation and commit strategy**

**Defining compaction concurrency and status handling**

**Specifying compaction model context handling**

**Confirming token limit details**
