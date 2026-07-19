# smol mesh authentication — a layered design for #190 (transport auth) + #184 (record signing)

**Issues:** [#190](https://github.com/jphein/smol/issues/190) (ESP-NOW PMK/LMK link auth — the #189 GAP, learned from Althea #12) · [#184](https://github.com/jphein/smol/issues/184) (on-device ed25519 record signing — the #181 L3 key-on-device gate)
**Companions:** [mesh-ledger study](../research/mesh-ledger-study.md) (#181/#182 — the sequencing/tamper-evidence layer this sits beside) · [Althea/Babel study](../research/althea-babel-study.md) (#163)
**Status:** research / design only — **no firmware**. JP decides the build order.
**Author:** morpheus-155 · **Date:** 2026-07-19

---

## 0. Thesis

The two issues **rhyme**: both add authenticity to a mesh that today trusts every byte it hears. But they operate at **different layers and defend different attackers**, so they are **one system with two rungs**, not one feature:

- **#190 — transport auth (ESP-NOW PMK/LMK).** A cheap, broad, symmetric shield: AES-CCM on *every* ESP-NOW frame in hardware. Keeps anyone **without the shared key** off the RF wire entirely. Defends the **outsider** (a stranger on ch1/6/11 with an ESP32). Cost: ~free (hw crypto), one shared secret.
- **#184 — record auth (ed25519 per-record signing).** An expensive, targeted, asymmetric proof: a 64-byte signature on the *few catastrophic records*. Makes those records **unforgeable even if the symmetric key leaks** or a board is extracted. Defends the **insider / key-compromise** for the records that can brick or hijack the fleet. Cost: 64 B/frame + signing CPU + the key-on-device decision.

**Layer them.** #190 is the front door lock (stops randoms cheaply); #184 is the safe inside (protects the crown jewels even past the front door). Ship #190 first (broad, cheap, no MTU cost); ship #184 for the short list of records that are worth 64 bytes. **smol already ships the #184 pattern for its single most dangerous frame** — the OTA arm — so the precedent and the code path exist.

---

## 1. Threat model

### 1.1 Two transports, two very different trust levels

| Path | Who can write to it | Current auth |
|---|---|---|
| **LAN / MQTT** (`smol/*` topics: MC `smol/mesh/channel`, `channel_hint`, `ota/staged`, `ota/install`, `config/*`) | anything with **broker credentials** | ✅ broker auth (creds in `secrets.rs`, git-ignored, **not** in the public repo) |
| **ESP-NOW RF mesh** (every `SMOLv1 …` frame) | **anyone within RF range on ch 1/6/11** | ❌ **none** — plaintext, unauthenticated, un-encrypted (`add_peer(… encrypt: false)`, mode.rs) |

The LAN origin of a record is reasonably protected (you need broker creds). **The RF mesh is wide open.** And critically: **MQTT-origin records get re-broadcast onto the unauth RF wire** — the gateway relays `config/*` as `SMOLv1 CFG` frames, and the crown's identity/channel propagate via `SMOLv1 HELLO`. So even a broker-authed record is re-exposed to forgery at the mesh relay hop. **The ESP-NOW wire is the attack surface both issues target.**

### 1.2 What an unauthenticated RF attacker can inject today

Anyone on the channel, with a $1 ESP32 and the (public-repo) frame formats, can forge:

| Forged frame | Effect | Severity |
|---|---|---|
| `SMOLv1 HELLO <id>` as the crown / a MC on the crown channel | **crown hijack / partition** — leaves lock to a phantom owner, mesh drags onto the attacker's channel | 🔴 catastrophic (this is the manual seq-forgery #155 was built to retire — now an *attacker* can do it) |
| `SMOLv1 CFG <id>R` (reboot) / config keys | **remote reboot / config tamper** — a forged retained-relayed reboot is a reboot-loop brick | 🔴 catastrophic |
| `SMOLv1 TIME <id> <far-future>` | clock poisoning (noted in-code: "can inject a TIME frame with an arbitrary, far-future" value) | 🟠 disruptive |
| OTA arm (`OTAM`) | would flash arbitrary firmware — **but already ed25519-gated, fail-closed** (see §2) | 🟢 already defended |
| `SMOLv1 FAM` handoff | steal/kill the Familiar (game-layer) | 🟡 nuisance |
| `SMOLv1 RELAY` / telemetry / DIAG / STAT | inject false sensor/roster data | 🟡 nuisance |
| `channel_hint` (my #155) | steer the fleet's channel — **MQTT-only, broker-authed**; not on the RF wire, so not directly forgeable (but see the crown MC it influences) | 🟢 broker-gated |

**Ranked catastrophic records** (the #184 short list): **crown MC/HELLO, remote-reboot & config INTENTs, OTA arm (done).** Everything else is nuisance-tier — do not spend 64 bytes on it.

### 1.3 Attacker classes (whom each rung stops)

1. **Passersby / opportunist** (no key, on the channel): stopped by **#190** alone.
2. **Key holder who read the public repo**: the repo is public but the PMK is **not in it** (lives in `secrets.rs`). #190 stops them too — *provided* the PMK stays out of git (the [smol public-repo topology] discipline).
3. **Physical board extraction / key leak** (the PMK is symmetric — one board's flash dump = the fleet's key): **only #184** protects the catastrophic records here, because ed25519 forgery needs the *private signing key*, which never ships to a leaf.
4. **A malicious/buggy board that already has all keys** (full BFT threat): **out of scope** — smol's trust model is "all the boards are mine" (ledger study §5.1). Neither rung tries to solve Byzantine collusion; that's correctly declined.

---

## 2. What smol already has (reuse, don't reinvent)

The ledger study §6 map applies directly — the primitives exist:

| Primitive | Where | Reuse for auth |
|---|---|---|
| **ed25519 VERIFY** (`verify_signature(msg, &[u8;64]) -> bool`, fail-closed, no-alloc/no-RNG) | `ota.rs:290` | The verify half of #184 — **already on-device**, already `espnow`-gated. Any signed record verifies through this exact fn. |
| **A hardcoded 32-B pubkey** (`OTA_SIGNING_PUBKEY`) | `ota.rs:281` | The centrally-authored-record trust root (model A, §6.1). A record class can reuse it or get its own pubkey. |
| **A live signed frame on the mesh** (`OTAM` = manifest `build\|size\|sha256hex` + 64-B sig; "the SOLE root of trust on the unauth mesh. Fail-closed.") | `ota_mesh.rs:599` | **The #184 pattern already ships** for the OTA arm — the highest-stakes frame. #184 = generalize this to the other catastrophic records. |
| **sha256** | `ota.rs`, deps | Tamper-evidence + the payload hash a signature covers. |
| **ESP-NOW peers, encryption-capable but off** (`PeerInfo { encrypt: false, … }`) | `mode.rs:3471/3561/4978` | The #190 change surface: flip to `encrypt: true` + PMK/LMK. |
| **`dl_seq`** (per-source monotonic strict-newer gate) | `net/wire.rs`, `net/mode.rs` | Replay defense — a signature without a sequence/nonce is replayable; `dl_seq` is the anti-replay smol already has. |
| **`secrets.rs`** (git-ignored creds, WiFi/MQTT) | `src/secrets.rs` | Where the **PMK** lives (symmetric secret, never committed — like the WiFi pass). |

**The only genuinely new firmware is (a) turning on ESP-NOW encryption (#190) and (b) on-device SIGNING (#184).** Verify, hash, sequence, and the signed-frame pattern are all in-tree.

---

## 3. Frame-size budget — the 250 B MTU is the design constraint

`ESP_NOW_MTU = 250` (`net/wire.rs:238`). An ed25519 signature is **64 B = 26 % of the whole MTU** before any hex-encoding. This is *the* constraint that decides which records can afford which auth. Three tiers:

| Tier | Mechanism | Wire cost | Use for |
|---|---|---|---|
| **A. Full signature** | 64-B ed25519 sig appended (binary, not hex — hex would be 128 B and blow the budget) | +64 B | The **catastrophic short list** only: OTA arm (done), crown MC, remote-reboot/config INTENT. These are **low-rate** (election/arm/command), so 64 B of airtime is affordable. |
| **B. Truncated MAC** | HMAC-sha256 truncated to 8–16 B, keyed by a shared secret | +8–16 B | Medium-rate records that want integrity+auth but can't spend 64 B (e.g. TIME, FAM). **But** a MAC is *symmetric* — same trust as #190's PMK, so if #190 is on, a separate MAC is redundant for outsider defense. Recommend: **skip MACs; let #190 cover these.** Listed for completeness. |
| **C. Chain-of-trust from a signed root** | Sign a *root* (the crown MC or a periodic "tree head", ledger study §5.2); derive trust for dependent records from the signed root + a cheap `prev-hash` link, no per-record sig | +~16 B (prev-hash) | **The scalable answer.** Sign the crown MC (rung A) once per election; the crown then vouches for the records it relays. Reuses the crown-as-sequencer + the ledger hash-chain. Avoids 64 B on every frame. |

**Design rule:** full sigs (A) only on the 3 catastrophic, low-rate records; everything else rides #190 (transport) + the chain-of-trust (C) from the signed crown root. **No record carries a signature unless it can brick or hijack the fleet.**

Budget check for the three signed records (all comfortably fit 250 B):
- **OTA arm (OTAM):** already ships with the 64-B sig — proven to fit.
- **Crown MC:** `MC|owner|ch|seq` ≈ 20 B + 64-B sig = ~84 B. Fits.
- **Reboot/config INTENT:** `CFG|id|R` ≈ small + 64-B sig ≈ ~80 B. Fits.

---

## 4. Rung #190 — ESP-NOW transport auth (PMK/LMK)

**Mechanism.** ESP-NOW natively supports AES-CCM: a 16-B **PMK** (Primary Master Key, one per device, fleet-wide shared) and an optional per-peer 16-B **LMK** (Local Master Key). With `encrypt: true` + a provisioned LMK, the radio authenticates+encrypts the payload in hardware — a frame without the key is dropped by the MAC layer before it reaches smol's parser.

**What it buys.** Every `SMOLv1` frame becomes outsider-unforgeable and unreadable for the cost of provisioning one shared key. It closes attacker classes 1 & 2 (§1.3) across the *entire* frame surface — HELLO, TIME, CFG, FAM, RELAY, everything — with **zero per-frame byte cost** (AES-CCM overhead is in the ESP-NOW header, not smol's 250-B payload).

**What it does NOT buy.** Symmetric ⇒ every board holds the same PMK. **One extracted board compromises the fleet's transport key** (class 3). And it authenticates the *link*, not the *record's author* — a valid-key board can still originate a forged-content frame. Hence #184 for the catastrophic records.

**The broadcast wrinkle (must design around).** smol's mesh is **broadcast-heavy** (HELLO, flood/UP2, RELAY all go to `BROADCAST_ADDRESS`). ESP-NOW LMK encryption is **unicast-peer-oriented**; broadcast frames use the PMK path / can't use per-peer LMK. So #190 v1 realistically = **PMK-encrypted broadcast** (all boards share the PMK; broadcast frames are PMK-protected) rather than per-peer LMK. This is the item to confirm against the esp-wifi API at implementation time (does `esp-wifi`'s ESP-NOW expose PMK-encrypted broadcast, or only unicast LMK?). **If broadcast can't be encrypted**, #190 degrades to "unicast frames encrypted, broadcast frames still open" — which leaves HELLO/MC/flood (the catastrophic ones) exposed, and pushes *all* the weight onto #184. **This single API question decides whether #190 is worth doing before #184.** Flag for a spike.

**Key distribution.** The PMK is a symmetric secret → `secrets.rs` (git-ignored), provisioned at flash time exactly like the WiFi password. Rotation = reflash the fleet with a new PMK (OTA can't rotate the key it's authenticated under without a careful two-key overlap window — see §7). Public-repo hygiene: the PMK **must never** land in a committed file (the [smol public-repo topology] rule — the repo is public).

---

## 5. Rung #184 — on-device ed25519 record signing

**Two signing models — pick per record class:**

### 5.1 Model A — centrally-authored, off-device key (the OTA model, already shipping)
One ed25519 keypair. **Private key lives off-device** (the build host / operator, `JP-authorized` per the OTA keygen). Boards hold only the 32-B pubkey and **verify**. Used for records **authored by the operator/build host**, not by leaves:
- **OTA arm** — done.
- **Remote reboot / config INTENT** — an operator command. Sign it off-device (or at the HA/publish layer) so a leaf can verify the command is JP's, not an RF attacker's. Reuses `verify_signature` verbatim.
- **channel_hint (#155)** — already broker-authed on the MQTT side; if it ever rides an unauth relay, model A signs it.

This model needs **no on-device signing** — it's pure verify, which already exists. **It closes the catastrophic INTENT records with today's code + a signing step in the publish pipeline.** This is the cheapest, highest-value slice of #184.

### 5.2 Model B — per-node self-authored key (the ledger model, the real new work)
Each board has its **own** keypair; its private key lives **on the board**; peers verify with that board's pubkey (distributed via a signed roster or learned + pinned). Used for records where **a leaf is the legitimate author** and we want "node X really wrote this":
- **Crown MC** — any board can win the election, so the *winner* must sign its own MC. This is the record that most needs it (crown hijack = §1.2's top threat) **and** the one that can't use model A (no single off-device author). → needs **on-device signing** = the #181 L3 / #184 "key-on-device gate."
- Per-node ledger events (anti-cheat, ledger study) — the study's actual target.

**The gate (#184's crux):** on-device signing means a **private key in every board's flash** on a public-hardware, physically-accessible device. Extraction risk is real (class 3). Mitigations to design: per-board keys (one extraction ≠ fleet forgery, unlike the symmetric PMK), key in a flash region excluded from OTA image dumps, and accepting that anti-cheat/authenticity is "raise the bar," not "unbreakable" (ledger study §5.2 — detection ≠ prevention). **JP's call whether crown-MC signing is worth an on-device private key**; if not, fall back to §5.3.

### 5.3 Fallback if on-device signing is declined — crown MC via model A + chain-of-trust
If we don't want per-board private keys: the operator-authored path already exists — an operator can publish a **signed** `smol/mesh/channel` (model A, off-device sig) and the crown/leaves verify it. This makes the *operator's* crown/channel decision unforgeable without any on-device key, at the cost of losing autonomous self-election authenticity (a board can still self-elect, but only an *operator-signed* MC is trusted for a forced steer — which composes perfectly with **my #155 `channel_hint`**: sign the hint, and the operator lever becomes cryptographically authenticated). **This is the recommended v1** — it gets crown-steer authenticity with zero on-device signing, deferring the L3 gate.

---

## 6. Key distribution & rotation

| Key | Type | Location | Distribution | Rotation |
|---|---|---|---|---|
| **PMK** (#190) | symmetric 16 B | `secrets.rs` (git-ignored), fleet-shared | flash-time provisioning | reflash fleet; OTA rotation needs a **two-key overlap window** (accept old+new PMK for one release) or it partitions mid-rollout |
| **OTA/INTENT signing key** (#184 model A) | ed25519, private **off-device** | build host / operator only | pubkey hardcoded in firmware (`OTA_SIGNING_PUBKEY` precedent) | new pubkey = a firmware release; sign the transition announce with the *old* key |
| **Per-node key** (#184 model B, if adopted) | ed25519, private **on-device** | generated per board (first boot → NVS), pubkey gossiped/pinned | signed roster from the crown, or trust-on-first-use + pin | per-board reflash / re-gen and re-distribute pubkey |

**Rotation is the hard part and must not brick the fleet** — see §7. The safe pattern for every key: **overlap window** — accept both old and new for one release, so a half-rolled fleet still verifies.

---

## 7. Mixed-fleet rollout / compat (#100-class discipline — the brick-avoidance rung)

**The hard invariant: an old board must not brick or partition when a signed/encrypted record appears, and a new board must not reject an old board into isolation during the rollout.** smol's compat rule (#100/#56): **additive + ignore-unknown**, verified against real mixed-fleet.

### 7.1 #190 (encryption) is the dangerous one — it's NOT additive
Encryption is a **hard cutover**: an encrypted frame is *dropped by the MAC layer* on a board that isn't keyed, and vice-versa. Turn encryption on in one board and it goes **deaf to the plaintext fleet** — an instant partition (exactly the #204 deafness disease, self-inflicted). Rollout options:
- **(a) Flag day** — flash the whole fleet in one pass (feasible: small fleet, USB/OTA). Simplest, but every board must land the keyed image or it's stranded.
- **(b) Dual-listen transition** — a transition release that accepts BOTH plaintext and PMK-encrypted frames (RX), TX plaintext until a config flag flips fleet-wide, then a second release that goes encrypt-only. Two releases, no partition window. **Recommended** — it's the OTA-safe path and mirrors the two-key overlap.
- Whichever: **the transition release must be canaried** (one board) and prove it still hears the plaintext fleet before the fleet-wide flip. This is a #204-class deafness risk — treat it with the same rigor (positive inbound proof, not "it associated").

### 7.2 #184 (signing) IS additive — the safe one
A signature is an **appended field**; an old board ignores the extra bytes (or the record is dual-published: unsigned for old, signed for new). Rollout:
- Old boards ignore the sig and accept the record on its existing (unauth) merits — **no brick, but also no new protection for them** (acceptable during transition).
- New boards **prefer** a valid sig; during transition they accept unsigned records too (soft-enforce), then a later release **hard-rejects** unsigned catastrophic records (`verify or drop`, like OTAM already does). Two-phase: **observe → enforce**, so a fleet that isn't fully signing yet doesn't lose its crown.
- The `verify_fail` counter (already in `ota_mesh.rs:481`) is the observability: watch it during the observe phase before flipping to enforce.

### 7.3 The composition trap (must verify)
Signing/encryption must not break the guards I and others just shipped: **#155 channel_hint gate, #146/#136/#137/#150 election guards, #204 self-heal.** A signed MC still has to flow through the same resolver; an encrypted transport still has to let the #204 detector see "sustained inbound." **Design so auth is a filter *in front of* the existing logic, never a rewrite of it** — verify-then-parse (the OTAM order), exactly as #184's verify gate already sits before `parse_manifest`.

---

## 8. What NOT to sign / encrypt (C3 perf & RAM budget)

The C3 is single-core RISC-V @160 MHz, ~400 KB SRAM, no PSRAM. ed25519 verify is ~ms-scale; **signing** is comparable but adds RNG + key handling. Budget rules:

- **DON'T sign** high-rate/nuisance frames: HELLO (2 Hz × N boards), BEACON, RELAY telemetry, DIAG, STAT, SCAN, FAM. Signing these would (a) blow airtime on the deaf-⅓ single radio and (b) burn CPU on a board already CPU-blocking during flush. **#190 covers them for free; that's sufficient for nuisance-tier.**
- **DON'T MAC** anything if #190 is on — a symmetric MAC duplicates the PMK's protection (§3 tier B).
- **DO sign** only: OTA arm (done), crown MC, remote-reboot/config INTENT — all **low-rate** (per-election / per-command).
- **RAM:** verify is no-alloc (proven, `ota.rs`); a per-node signing key adds 32 B priv + 32 B pub in NVS — negligible. The 64-B sig buffers are stack, sized like the existing OTAM path.
- **Airtime is the real budget, not CPU:** the [smol retire-the-burst] deaf window means every extra broadcast byte matters. This is why §3's chain-of-trust (sign the root, not every record) is the scalable answer.

---

## 9. Recommended build order (JP decides)

A dependency-ordered menu, cheapest-highest-value first:

1. **#184 model A for INTENTs (no new crypto).** Sign remote-reboot/config commands off-device; boards verify with the existing `verify_signature`. Closes the reboot-brick injection with today's code + a publish-side signing step. **Cheapest catastrophic-record win.**
2. **Sign the #155 channel_hint / crown-steer (model A).** Make the operator lever cryptographically authenticated — small, composes with shipped #155.
3. **#190 spike:** answer the broadcast-encryption API question (§4). If PMK-encrypted broadcast works → do the dual-listen rollout (§7.1b). If not → #190 is unicast-only and lower-value; weight shifts to #184.
4. **#184 model B / crown-MC signing** — only if JP wants autonomous self-election authenticity and accepts the on-device-key gate (§5.2); else the §5.3 model-A fallback covers crown-steer.
5. **Chain-of-trust / signed tree-head** (ledger study §5.2) — the scalable layer, if the ledger (#181/#182) is built.

**No implementation in this doc.** Each rung is independently shippable and independently canary-gated.

---

## 10. Relationship to the mesh-ledger (#181/#182)

This auth design and the ledger are the **same coin**: the ledger study's "authenticity rung" **is** #184. Concretely:
- The ledger's `prev-hash` chain = tamper-**evidence** (sha256, free today).
- #184 signing = tamper-**proof** (the study's §5/§9 "on-device signing gate").
- The crown-as-sequencer + signed tree-head (study §5.2) is the §3-C chain-of-trust that lets us sign *one root* instead of every record.
- `dl_seq` (per-source monotonic) is the shared anti-replay both need.

**If the ledger is built, #184 rides it (sign the log records). If not, #184 stands alone for the 3 catastrophic records.** Design them so the signed-record envelope is the same shape either way (verify-then-parse, 64-B binary sig suffix, sequence/nonce for replay).

---

## 11. Open questions (for JP / the spike)

1. **#190 broadcast encryption** — does `esp-wifi` ESP-NOW support PMK-encrypted *broadcast*, or only unicast LMK? (Decides #190's value; §4.) **Highest-priority unknown.**
2. **On-device key gate (#184 model B)** — is autonomous self-election authenticity worth a per-board private key on physically-accessible hardware? Or is the model-A operator-signed-steer fallback (§5.3) enough for v1?
3. **Rotation appetite** — flag-day reflash vs the two-release overlap window (§7)? Affects whether #190 is a weekend or a two-wave rollout.
4. **Enforce threshold** — after the observe phase, what `verify_fail` rate / soak duration gates the flip from soft-accept to hard-reject-unsigned?
5. **Replay** — is `dl_seq` sufficient anti-replay for signed INTENTs, or does a reboot/command need a nonce + freshness window (a signed reboot captured off the air could be replayed later)?

---

*Design only. The one thing already true today: smol ed25519-verifies its most dangerous frame and fails closed. #184 is "do that for two more records"; #190 is "lock the front door so we rarely have to." Ship the cheap catastrophic-record wins (§9.1–2) first; spike the #190 broadcast question before committing to the encryption rollout.*
