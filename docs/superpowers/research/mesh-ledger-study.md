# A tamper-evident / append-only ledger ON the smol mesh — a study

**Issue:** [#181](https://github.com/jphein/smol/issues/181) · **Companion to:** [#163 Althea/Babel study](althea-babel-study.md) · **Status:** research/design only, no firmware · **Author:** nebula-babel · **Date:** 2026-07-18

> **Bottom line up front.** smol does **not** want a blockchain, a BFT quorum, or a full
> CRDT engine — all three solve problems it doesn't have (Byzantine peers, coordinator-free
> convergence at massive scale, arbitrary-JSON conflict resolution), at a cost a C3 can't
> pay. What smol wants is the *cheapest thing that makes its history tamper-EVIDENT and
> replayable*: a **per-node hash-chained append-only log**, gossiped over the flood/UP2
> transport it already has, keyed by the per-source sequence it already half-invented
> (`dl_seq`) and anchored by the durable monotonic counter it already ships (`boot_count`).
> sha256 is already in-tree (~4 KB, OTA). That's the ADOPT.
>
> **The single sharpest finding — the crypto isn't as "already in-tree" as the issue
> assumes.** Ed25519 on the C3 is compiled **verify-only**; OTA *signing* happens
> off-device (`tools/ota_publish.sh`, key deliberately *never on the board*). A per-node
> *signed* log needs each board to **sign its own events on-device** — a code path that
> isn't compiled *and* a security-posture change (a secret key on every board). So the
> tamper-**evident** layer (hash chain, sha256) is free today; the tamper-**proof** layer
> (ed25519 signatures) is a real, separable v2 decision. Split them.
>
> **The gem — smol already half-built the ledger, and it already has the thing BFT works
> hardest to get.** `dl_seq` ≈ a per-source sequence number, `boot_count` ≈ a
> power-loss-safe monotonic epoch, retained-MQTT ≈ a Last-Writer-Wins register (the
> simplest CRDT), the flood **seen-set** ≈ gossip dedup/anti-entropy — and the **#76 crown
> election is a free sequencer**. A ledger here is *wiring these together*, exactly as the
> [#163 ETX metric](althea-babel-study.md) *finished* the half-built BEACON `echo`.

---

## 0. Provenance & method (verify, don't assume)

Grounded in source + primary references read directly, not recalled:

| Source | What was read | License |
|---|---|---|
| smol tree | `net/wire.rs` (UP2 MTU budget, `dl_seq`), `net/mode.rs` (dl_seq gate, seen-set, crown), `ota.rs` (`boot_count` dual-cell durable write, ed25519 **verify-only**), `Cargo.toml` (crypto deps + the #32 build gate), `net/cast.rs`/`mqtt.rs` (retained-MQTT LWW) | — |
| [Secure Scuttlebutt protocol guide](https://ssbc.github.io/scuttlebutt-protocol-guide/) | per-feed signed hash-chain structure, ed25519+sha256, EBT replication, no-global-consensus model | — (docs) |
| [Merkle-CRDTs (arXiv 2004.00107)](https://arxiv.org/abs/2004.00107) | Merkle-DAG-as-CRDT-transport, Merkle-Clock causal order, content-addressed dedup | paper |
| `automerge/automerge` (clone) | full JSON CRDT — **101,299 LoC**, edition 2021, `Vec`/std-heavy, `.save()→Vec<u8>` | MIT |
| `orbitdb/orbit-db` (clone) | Merkle-DAG log over IPFS (the "distributed DB" shape) | MIT |
| RFC 6962 (Certificate Transparency) | append-only Merkle tree, Signed Tree Head, O(log n) consistency proofs (recalled; the tamper-evident-log gold standard) | IETF |

Clones live **outside** the repo in `~/Projects/mesh-ledger-study/` (both MIT) — study
material, never committed, mirroring the [#163](althea-babel-study.md) hygiene.

---

## 1. What smol already has — the ledger is half-built

Before designing anything, name the primitives already in the tree. A smol-ledger is mostly
*assembly*, not invention:

| smol primitive | Where | Ledger concept it already is |
|---|---|---|
| **`dl_seq`** (10-digit monotonic, strict-newer re-flood gate on BATT2/GRID2) | `net/wire.rs`, `net/mode.rs` | A **per-source sequence number** + a monotonic freshness gate — SSB's `sequence` field and Babel's seqno, applied to the downlink. Adopt/replay a record *iff strictly newer*. |
| **`boot_count`** (NVS dual-cell, torn-write-safe, power-loss-safe, monotonic) | `ota.rs::boot_count_bump` | A **durable monotonic epoch** — survives power loss via a two-cell "write the smaller cell, keep the other" scheme. Exactly the durable-head write a log needs so a crash can't lose or rewind the chain tip. |
| **retained MQTT** (`smol/<id>/cast`, `/screen`, `smol/display/batt`) | `net/cast.rs`, `net/mqtt.rs` | A **Last-Writer-Wins register** — the simplest CRDT. The broker holds the latest value; a reconnecting node reads current state. smol already runs one LWW register per topic. |
| **flood seen-set** `(origin, msgid, frag)` | `net/flood.rs` | **Gossip dedup / anti-entropy** — the "have I already seen+forwarded this?" guard every gossiped log needs to terminate. |
| **#76 crown election** | `net/mode.rs` | A **sequencer / single elected leader** — the expensive thing BFT and Raft exist to produce. smol elects one for free and re-elects on failure. |
| **ed25519 verify + sha256** | `ota.rs`, `Cargo.toml` | Tamper-evidence (sha256) + authenticity *verification* (ed25519) — but see §5: **signing is off-device**. |

The implication is the same as the ETX finding in [#163](althea-babel-study.md): the
adoptable work is *finishing* what smol started, not grafting a foreign system.

---

## 2. The design space — a tamper-evidence spectrum, not a binary

"Ledger" spans a spectrum of guarantees; each rung costs more. Place smol's needs on it:

| Rung | Guarantee | Mechanism | smol relevance |
|---|---|---|---|
| Freshness | "this is the latest" | monotonic seq (LWW) | **Already have it** (`dl_seq`, retained-MQTT) |
| **Integrity / tamper-evident** | "the history wasn't silently altered/reordered/truncated" | **hash chain** (each record links prev-hash) | **The target** — sha256 in-tree, cheap |
| Authenticity / tamper-proof | "node X really wrote this, no forgery" | **signatures** (ed25519 per record) | Valuable for anti-cheat; gated on on-device signing (§5) |
| Total order across nodes | "everyone agrees on the sequence" | sequencer **or** consensus | Cheap via the **crown** (already elected); expensive via BFT |
| Byzantine agreement | "correct even if k nodes lie/collude" | BFT quorum (PBFT/Tendermint) | **Not a smol threat** — you own every board |

The whole study reduces to: smol wants the **integrity** rung (cheap, adopt now), *optionally*
the **authenticity** rung (gated), can get **total order** almost free from the crown, and
should **not** climb to Byzantine agreement.

---

## 3. Option 1 — per-node hash-chained append-only log (SSB-lineage)

Each board keeps **its own** append-only log. Each record links the previous record's hash,
so any insertion/deletion/reorder breaks the chain and is detectable by replay. Records are
gossiped over the existing flood/UP2 transport; peers cache others' chains.

### 3.1 Mechanics (from SSB, adapted)
SSB is the mature reference: a per-identity feed of `{previous(msgID), author, sequence,
timestamp, hash, content, signature}`, ed25519-signed, sha256-hashed, **no global
consensus — per-feed total order only**, gossiped by EBT (vector-clock) replication. That is
*exactly* the per-node model — SSB proves it works offline-first.

### 3.2 The MTU problem (the sharpest C3 constraint)
SSB messages carry ~130 B of crypto metadata (32 B prev-hash + 32 B author pubkey + 64 B
sig + seq/ts). On smol's **227 B UP2 inner budget** (`ESP_NOW_MTU 250 − UP2_OVERHEAD 23`),
that leaves <100 B for content — and a full 32 B author pubkey per frame is wasteful when the
ESP-NOW source MAC + smol node-id already identify the author. **A smol-ledger record must
use compact crypto:**

| Field | SSB | smol-ledger (proposed) | Note |
|---|---|---|---|
| author | 32 B pubkey | **0 B** — the 1-B node-id / source MAC identifies it | pubkey looked up once from a roster |
| sequence | var | reuse **`dl_seq`** style (fits in existing counters) | per-node monotonic |
| prev-hash | 32 B | **16 B truncated sha256** | 2⁻¹²⁸ collision — ample for a 30-node fleet |
| content | — | ≤ ~150 B | fits the real events (election, OTA install, sensor tick, RPG world-event) |
| signature | 64 B | **optional (v2)** — omit in v1 | see §5 |

**v1 (unsigned) record ≈ 16 B prev-hash + seq + content — fits UP2 comfortably.** This is the
key move: split integrity (cheap, fits) from authenticity (§5).

### 3.3 C3 budget
- **Flash:** the log is append-only and must be **bounded** — a ring in a dedicated flash
  region with prune-oldest (smol already ring-prunes; `boot_count` shows the durable-write
  pattern). Say 8–32 KB/node of recent history; older records age out (or the crown keeps a
  longer archive via MQTT→HA). Tamper-evidence survives pruning if each prune publishes the
  pruned-prefix's terminal hash (a checkpoint).
- **RAM:** a chain *head* (16 B last-hash + seq) per known node × ~16 roster slots ≈ **~300 B**.
  Verifying a newly-heard record = one sha256 over ≤227 B (~µs). Trivial.
- **Airtime:** each append is one broadcast frame, deduped by the existing seen-set, rides the
  flood exactly like telemetry. **No new periodic cost** — events are already being emitted
  (elections, OTA, telemetry); the ledger just adds a prev-hash+seq header to make them a
  chain. This is the crucial airtime point: a hash-chain is *free-riding* on frames smol
  already sends.
- **no_std:** sha256 (`sha2`, in-tree) is no_std; the chain logic is integer + slice work,
  host-testable like `flood.rs`/`etx.rs`.

**Verdict: ADOPT.** Cheapest, most smol-native, finishes `dl_seq`/`boot_count`/seen-set,
free-rides existing airtime, delivers fleet provenance immediately.

---

## 4. Option 2 — CRDT / Merkle-DAG gossip-merged log

A conflict-free replicated log: any node appends, states merge deterministically without a
coordinator. Two very different weights hide under this heading — separate them:

### 4.1 G-Set / delta-CRDT (light) — ADOPT-lite for *specific* shared state
A **grow-only set** (G-Set) is the simplest CRDT: add-only, merge = set **union**.
Conflict-free because union is commutative, associative, and idempotent — so merge order and
duplicate delivery don't matter (which is *exactly* smol's lossy-flood reality). A
**delta-CRDT** ships only the new elements, not the whole set — bandwidth-friendly. This is
tiny (a bounded set + a union op), no_std-trivial, needs no coordinator, and is the right
tool for **multi-writer** shared state where several boards independently contribute:
- the RPG's discovered-loot set / world-events set / gravestone registry,
- a fleet-wide "which boards have seen event X" set.

**Verdict: ADOPT-lite**, but only where multi-writer convergence is genuinely needed. For
single-author provenance (who-did-what), the Option-1 per-node chain is simpler and also
gives *order*, which a G-Set deliberately discards.

### 4.2 Merkle-DAG / full CRDT (heavy) — ADMIRE
Merkle-CRDTs (arXiv 2004.00107) use a Merkle-DAG as transport+persistence: content-addressed
nodes, a **Merkle-Clock** deriving causal order from DAG structure (no synced clocks —
elegant), dedup via content-addressing, sync by fetching missing nodes by hash. It's built
for "**a very large number of replicas**" with "weak messaging guarantees" — the IPFS/OrbitDB
scale. At smol's **30 co-located nodes with an elected crown**, the DAG machinery (random
fetch-by-hash, DHT-style discovery, DAG traversal/GC) is far more than needed. And
**automerge** — the mature full-JSON CRDT — is **101 K LoC, `Vec`/std-heavy, alloc-first,
`.save()→Vec<u8>`**: categorically no_std-hostile, the *Rita-of-ledgers* (cf. [#163](althea-babel-study.md)'s
"nothing in the Althea stack ports"). 

**Verdict: ADMIRE.** Borrow the *Merkle-Clock causal-order idea* conceptually; don't port the
DAG/automerge machinery.

---

## 5. Option 3 — light-BFT / quorum — and why the crown beats it

### 5.1 BFT is solving a threat smol doesn't have
PBFT/Tendermint/HotStuff tolerate **Byzantine** (lying, colluding) participants via quorum
rounds — typically O(n²) messaging, stable membership, and multiple round-trips per commit.
smol's trust model is **"all the boards are mine."** The adversary BFT defends against
(a subset of your own co-located boards actively colluding to forge history) is not smol's
threat. Layer that on a **single-radio mesh that goes deaf ⅓ of the time** ([#163](althea-babel-study.md))
and O(n²) quorum rounds are both unnecessary and unaffordable.

### 5.2 smol already has the cheap 90% — the crown as sequencer
The reason BFT/Raft are expensive is **leader election + agreement**. smol **already elects a
leader** (#76 crown) and re-elects on failure. So if smol ever wants a *total order* across
nodes (not just per-node order), the crown is a **free sequencer**: nodes send appends to the
crown; the crown assigns global positions and periodically publishes a **"tree head"** — a
hash over the merged log heads, exactly like Certificate Transparency's **Signed Tree Head**
(RFC 6962) — using the **existing `dl_seq` + retained-MQTT** downlink it already runs. A node
compares its view against the crown's tree head to detect divergence in O(1). No quorum, no
BFT, reusing two mechanisms smol already ships.

- If the crown is honest (it's your board): you get single-writer total order + a global
  integrity anchor for the cost of one periodic broadcast.
- If you distrust the crown too: that's the authenticity rung (§ per-node signatures) —
  still not full BFT, because detection (everyone can verify the chain) ≠ prevention.

**Verdict on BFT: DON'T** (overkill, wrong threat, unaffordable). **Verdict on crown-ordered
tree-head checkpoint: ADOPT-lite** — a cheap global consistency anchor that reuses the crown +
`dl_seq` + retained-MQTT.

---

## 6. Convergent design — smol already half-built it

The reassuring pattern from [#163](althea-babel-study.md) repeats: where smol needed a
ledger mechanism, it independently grew one. A smol-ledger *connects the dots*:

| Ledger mechanism | smol's existing equivalent | Gap to close |
|---|---|---|
| Per-source sequence (SSB `sequence`) | **`dl_seq`** strict-newer gate | Generalise from downlink-only to any event stream |
| Durable monotonic head (crash-safe) | **`boot_count`** dual-cell NVS write | Reuse the pattern for the chain tip |
| LWW register (simplest CRDT) | **retained MQTT** per topic | Already correct; name it |
| Gossip dedup / anti-entropy | flood **seen-set** `(origin,msgid,frag)` | Already correct; the log rides it |
| Sequencer / leader | **#76 crown election** | Add "publish a tree-head checkpoint" |
| Tamper-evidence | sha256 (OTA) | Add a prev-hash field to event frames |
| Authenticity | ed25519 **verify** (OTA) | Add on-device **sign** (the real new work — §5/§9) |

---

## 7. The C3 budget, side by side

| Option | Flash | RAM | Airtime | MTU fit (250 B) | no_std | Verdict |
|---|---|---|---|---|---|---|
| **1. Hash-chain per-node (unsigned)** | bounded ring, ~8–32 KB/node | ~300 B (heads) | **free-rides existing frames** | ✅ (16 B hash + seq + content) | ✅ sha2 in-tree | **ADOPT** |
| 1b. + ed25519 signatures | +~4 KB code (sign path) | +64 B/record | +64 B/frame → tighter | ⚠️ 64 B sig eats the budget | ✅ but sign not compiled | **ADOPT-lite (gated, §9)** |
| 2a. G-Set / delta-CRDT | small (bounded set) | bounded set | delta-only, cheap | ✅ | ✅ trivial | **ADOPT-lite (specific state)** |
| 2b. Merkle-DAG / automerge | huge | alloc-heavy | fetch-by-hash chatter | ✅ per node, ✗ overall | ✗ (101 K LoC std) | **ADMIRE** |
| 3. light-BFT / quorum | moderate | membership+rounds | **O(n²) on a deaf radio** | quorum msgs | hand-roll only | **DON'T** |
| 3b. crown-ordered tree-head | tiny | tiny | 1 periodic broadcast | ✅ (reuses dl_seq) | ✅ | **ADOPT-lite** |

---

## 8. Recommended minimal "smol-ledger" shape

A layered design where each layer is independently shippable and gated:

1. **v1 — the chain (ADOPT).** Add an optional `{prev16, seq}` header to event frames that
   matter (elections, OTA installs, config changes; later RPG world-events). Each node
   maintains its own chain; the tip is stored with the `boot_count` dual-cell durable-write
   pattern. Records gossip over flood/UP2, deduped by the seen-set. Replay detects tamper.
   sha256 only. **Cost: ~300 B RAM, a bounded flash ring, zero new airtime.** Host-testable
   pure module (`ledger.rs` + `ledger_verify`), exactly like `etx.rs`.
2. **v1.5 — the crown tree-head (ADOPT-lite).** The elected crown periodically publishes a
   hash over the heads it has merged (a CT-style Signed-Tree-Head, minus the signature in v1),
   via the existing `dl_seq`+retained-MQTT downlink. Gives a global consistency anchor + lands
   the whole fleet's provenance in HA for free.
3. **v2 — signatures (ADOPT-lite, gated).** Add on-device ed25519 **signing** for authenticity
   (anti-cheat, loot-provenance). **This is the real new work + a security decision** (§9).
4. **CRDT G-Set (ADOPT-lite, orthogonal).** For genuinely multi-writer shared state (RPG loot
   set, gravestone registry), a delta-G-Set alongside the per-node chains.

---

## 9. The honest gate — on-device signing

The issue's premise "Ed25519 already in-tree (OTA sig)" is **half true and worth stating
plainly**: on the C3, ed25519 is compiled **verify-only**, over *public* OTA manifests; the
signing key lives **off-device** (`tools/ota_publish.sh`, "never on disk" on the board — a
deliberate security posture). A *signed* per-node ledger requires:
- the **sign** code path compiled in (not today; ~KB, and note the [#32 build-gate](../../.. ) —
  ed25519-compact's scalar-mult unrolls to ~1 MB without the `opt-level="z"` per-package trick;
  the sign path needs the same care),
- a **per-board secret key on the C3** — every board becomes a signing oracle; if one is
  physically compromised, its identity is forgeable. For "trust among your own boards" that's
  usually fine, but it's a *decision*, not a freebie.

Therefore: **ship v1 unsigned (tamper-evident) first; make signatures a gated v2.** Hash-chain
integrity already catches accidental corruption, reorder, truncation, and any tampering by a
node that doesn't also control the downstream readers — which covers the headline use case
(fleet provenance / audit trail) completely.

---

## 10. What it enables (why the ranking is worth it)
- **Fleet provenance** — a tamper-evident, replayable OTA-install + election + config-change
  history. *Tonight's OOM-cascade-and-recovery saga is the perfect use case*: "which board
  rebooted, re-elected, and OTA'd when" becomes an auditable chain, not a guess from logs.
- **Mesh-RPG (2026-07-15 spec)** — a shared append-log is the natural world-state substrate:
  item/loot provenance (where did this drop come from), the gravestone economy (a
  tamper-evident death/recovery record), anti-cheat (a forged inventory breaks its chain /
  fails its signature). [#163](althea-babel-study.md) flagged deep many-destination mesh
  (RPG world routing) as exactly where heavier machinery earns its keep — the ledger is that
  machinery, kept smol-sized.
- **Offline-first** — per-node chains survive partitions and reconcile on heal by exchanging
  missing suffixes (SSB's model), formalising what smol already does ad-hoc with retained
  checkpoints.

---

## 11. Ranked adopt-vs-admire

**ADOPT**
1. **Per-node hash-chained append log, unsigned (v1)** — tamper-evident, sha256-only,
   free-rides existing airtime, finishes `dl_seq`/`boot_count`/seen-set. *The prize.* → issue.

**ADOPT-lite**
2. **Crown-ordered tree-head checkpoint** — CT-style global consistency anchor via the crown +
   `dl_seq` + retained-MQTT; a free sequencer instead of BFT. → issue.
3. **On-device ed25519 signatures (v2)** — authenticity for anti-cheat/loot-provenance; gated
   on the sign-path + key-on-device decision (§9). → issue.
4. **Delta-G-Set CRDT for specific multi-writer state** — RPG loot/gravestone/world-event
   sets; conflict-free union merge, tiny. → issue.

**ADMIRE** (correct elsewhere, wrong for smol — documented, not filed)
5. **Merkle-DAG / automerge full CRDT** — the Merkle-Clock causal-order idea is elegant, but
   automerge is 101 K LoC alloc-heavy std, and the DAG machinery targets massive replica
   counts smol doesn't have. Borrow the idea, admire the engine.

**DON'T**
6. **Light-BFT / quorum (Raft/PBFT/Tendermint)** — Byzantine tolerance solves a threat smol
   doesn't have (you own every board); O(n²) rounds on a deaf single-radio mesh; and the crown
   already gives cheap sequencing. The full-Babel of this study.

---

## 12. Follow-up issues
Per *research-findings-become-issues*, the ADOPT/-lite items are filed as their own issues,
linked back here:
- **L1 — [#182](https://github.com/jphein/smol/issues/182)** — per-node hash-chained append
  log (unsigned v1, tamper-evident). *The prize; the foundation for L2–L4.*
- **L2 — [#183](https://github.com/jphein/smol/issues/183)** — crown-ordered tree-head
  checkpoint (CT-style global anchor, no BFT). *Depends on #182.*
- **L3 — [#184](https://github.com/jphein/smol/issues/184)** — on-device ed25519 signing (v2
  authenticity) — the key-on-device security gate. *Depends on #182; gated.*
- **L4 — [#185](https://github.com/jphein/smol/issues/185)** — delta-G-Set CRDT for
  multi-writer shared state (RPG loot/gravestone). *Orthogonal to #182.*

ADMIRE/DON'T (5–6) are deliberately **not** filed — this section records why they were
considered and declined.

---

## 13. Executive summary
smol doesn't need a blockchain; it needs the cheapest thing that makes its history
tamper-evident, and it has already grown most of the parts. A **per-node hash-chained
append-only log** — sha256 (in-tree), a 16-byte prev-hash + a per-node sequence
(generalising `dl_seq`), a crash-safe tip (reusing `boot_count`'s dual-cell write), gossiped
over the flood/UP2 transport and deduped by the existing seen-set — delivers tamper-evident
fleet provenance for ~300 bytes of RAM, a bounded flash ring, and **zero new airtime** (it
free-rides frames smol already sends). Total order across nodes, if ever wanted, comes almost
free from the **crown as sequencer** publishing a Certificate-Transparency-style tree-head over
the downlink smol already runs — no BFT, which solves a Byzantine threat smol doesn't have.
The one genuine new cost is **on-device signing**: ed25519 is in-tree *verify-only*, the key
is deliberately off-board, so *authenticity* (anti-cheat, loot-provenance) is a separable,
gated v2 — while *integrity* (the audit trail, the OOM-saga replay, the RPG world-state
substrate) ships in v1 with primitives smol already owns. **Adopt the hash chain; let the
crown sequence it; sign it later; admire the CRDT engines; skip BFT.**
