# smol mesh authentication — a layered design for #190 (transport auth) + #184 (record signing)

**Issues:** [#190](https://github.com/jphein/smol/issues/190) (ESP-NOW PMK/LMK link auth — the #189 GAP, learned from Althea #12) · [#184](https://github.com/jphein/smol/issues/184) (on-device ed25519 record signing — the #181 L3 key-on-device gate)
**Companions:** [mesh-ledger study](../research/mesh-ledger-study.md) (#181/#182 — the sequencing/tamper-evidence layer this sits beside) · [Althea/Babel study](../research/althea-babel-study.md) (#163)
**Status:** research / design only — **no firmware**. JP decides the build order.
**Author:** morpheus-155 · **Date:** 2026-07-19

> ### ✏️ AMENDMENT — 2026-07-19 (post-#36 spike, nebula) — the transport rung is reshaped per **Fork B**
> §4 flagged one question as *"the single API question that decides whether #190 is worth
> doing"* and marked it **"flag for a spike."** [#36](https://github.com/jphein/smol/issues/206)
> ran that spike and **answered it decisively: ESP-NOW native encryption is unicast-LMK
> only — PMK-encrypted broadcast does not exist.** smol's mesh is broadcast-heavy, so the
> original #190 (hardware AES-CCM on every frame) is **physically not buildable**. This
> amendment **reshapes the transport rung** (§4, rewritten below) from *encryption* to
> *authentication*: an app-layer **group HMAC-SHA256** over broadcast payloads. It also
> updates the §0 thesis bullet, the §3 tier table, the §6 key row, §7.1 rollout, and §8.
> The three citations are inline in §4. **No build decision — that stays JP's; this removes
> the impossible option so the choice is clean.** Two happy side-effects: the transport
> rung stops being the "dangerous non-additive" one (§7.1), and it now *composes* with #184
> instead of overlapping it (§4 B2).

---

## 0. Thesis

The two issues **rhyme**: both add authenticity to a mesh that today trusts every byte it hears. But they operate at **different layers and defend different attackers**, so they are **one system with two rungs**, not one feature:

- **#190 — transport auth (group HMAC-SHA256 over broadcast payloads).** *(Reshaped per Fork B — see the amendment + §4.)* A cheap, broad, **symmetric authenticity** shield: a truncated HMAC appended in software (the ESP-NOW hardware **cannot** encrypt broadcast — #36). Keeps anyone **without the shared group key** off the mesh (their frames fail the MAC and are dropped at parse). Defends the **outsider** (a stranger on ch1/6/11 with an ESP32). Cost: +8–16 B/frame, one shared secret, sha256 (already in-tree). **Authenticity, not confidentiality** (payloads stay plaintext — see §4's confidentiality-SKIP).
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
2. **Key holder who read the public repo**: the repo is public but the GROUP_KEY is **not in it** (lives in `secrets.rs`). #190 stops them too — *provided* the key stays out of git (the [smol public-repo topology] discipline).
3. **Physical board extraction / key leak** (the GROUP_KEY is symmetric — one board's flash dump = the fleet's key, so the extractor can forge MAC'd frames): **only #184** protects the catastrophic records here, because ed25519 forgery needs the *private signing key*, which never ships to a leaf.
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
| **ESP-NOW peers** (`PeerInfo { encrypt: false, … }`) | `mode.rs:3471/3561/4978` | ~~The #190 surface: flip to `encrypt: true`~~ — **DEAD (Fork B/#36): `encrypt: true` can't apply to broadcast.** The reshaped #190 surface is instead the **frame encode/decode path** (`net/wire.rs`) — append/verify the group MAC there. `encrypt` stays `false`. |
| **`dl_seq`** (per-source monotonic strict-newer gate) | `net/wire.rs`, `net/mode.rs` | Replay defense — a signature without a sequence/nonce is replayable; `dl_seq` is the anti-replay smol already has. |
| **`secrets.rs`** (git-ignored creds, WiFi/MQTT) | `src/secrets.rs` | Where the **GROUP_KEY** lives (symmetric HMAC secret, never committed — like the WiFi pass). |

**The only genuinely new firmware is (a) computing/verifying the group MAC at the frame codec (#190, reshaped) and (b) on-device SIGNING (#184).** Verify, hash (sha256!), sequence, and the signed-frame pattern are all in-tree — and the MAC reuses the same sha256 that's already in the OTA path.

---

## 3. Frame-size budget — the 250 B MTU is the design constraint

`ESP_NOW_MTU = 250` (`net/wire.rs:238`). An ed25519 signature is **64 B = 26 % of the whole MTU** before any hex-encoding. This is *the* constraint that decides which records can afford which auth. Three tiers:

| Tier | Mechanism | Wire cost | Use for |
|---|---|---|---|
| **A. Full signature** | 64-B ed25519 sig appended (binary, not hex — hex would be 128 B and blow the budget) | +64 B | The **catastrophic short list** only: OTA arm (done), crown MC, remote-reboot/config INTENT. These are **low-rate** (election/arm/command), so 64 B of airtime is affordable. |
| **B. Truncated group MAC** | HMAC-sha256 truncated to 8–16 B, keyed by a shared group secret | +8–16 B | **⚠️ AMENDED (Fork B): this tier IS the reshaped #190 now.** The old note said "skip MACs; #190 covers these" — that assumed #190 = hardware AES-CCM. #36 proved broadcast can't be encrypted, so the group MAC is **no longer redundant — it's the primary outsider shield for broadcast** (HELLO/flood/TIME/FAM). Every broadcast frame carries it. See §4. |
| **C. Chain-of-trust from a signed root** | Sign a *root* (the crown MC or a periodic "tree head", ledger study §5.2); derive trust for dependent records from the signed root + a cheap `prev-hash` link, no per-record sig | +~16 B (prev-hash) | **The scalable answer.** Sign the crown MC (rung A) once per election; the crown then vouches for the records it relays. Reuses the crown-as-sequencer + the ledger hash-chain. Avoids 64 B on every frame. |

**Design rule:** full sigs (A) only on the 3 catastrophic, low-rate records; everything else rides #190 (the group MAC, tier B) + the chain-of-trust (C) from the signed crown root. **No record carries a 64-B signature unless it can brick or hijack the fleet — but every broadcast frame carries the cheap group MAC (tier B).** The catastrophic 3 carry **both** (MAC for the cheap outsider drop, ed25519 for insider/key-leak non-repudiation — they compose, §4 B2).

Budget check for the three signed records (all comfortably fit 250 B):
- **OTA arm (OTAM):** already ships with the 64-B sig — proven to fit.
- **Crown MC:** `MC|owner|ch|seq` ≈ 20 B + 64-B sig = ~84 B. Fits.
- **Reboot/config INTENT:** `CFG|id|R` ≈ small + 64-B sig ≈ ~80 B. Fits.

---

## 4. Rung #190 — broadcast transport **authentication** (group HMAC-SHA256) — *reshaped per Fork B (#36)*

> **SUPERSEDES the original §4.** The original rung was ESP-NOW hardware AES-CCM (PMK/LMK
> encryption). [#36](https://github.com/jphein/smol/issues/206) proved that is **not
> buildable for smol** (broadcast can't be encrypted). This §4 replaces it with an
> app-layer group MAC. (The original PMK/LMK text is preserved in git history at the parent
> of this amendment.)

### 4.0 Why the original PMK/LMK rung is dead — the three citations (#36)

ESP-NOW native encryption is **unicast-LMK only; there is no PMK-encrypted broadcast.**
Triangulated across the whole stack:

1. **Espressif ESP-IDF docs (authoritative):** *"Encrypting multicast vendor-specific
   action frame is not supported."* The PMK "is used to encrypt LMK with AES-128" (a
   *key-encryption-key*, not a broadcast data key); the LMK "encrypts the vendor-specific
   action frame" **per unicast peer**; "if the LMK … is not set, the frame will not be
   encrypted." Encrypted peers cap at **≤17 (default 7)** of **20** total.
   ([ESP-NOW API ref](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/network/esp_now.html))
2. **ESP-IDF blob header** (`esp_now.h`, pinned in the NuttX 3rdparty): `ESP_NOW_MAX_ENCRYPT_PEER_NUM 6`;
   `esp_now_set_pmk` only "encrypts the local master key"; `esp_now_fetch_peer` explicitly
   *"ignores the multicast/broadcast address."*
3. **esp-wifi 0.15 Rust API** (`esp_now/mod.rs`): exposes `set_pmk(&[u8;16])` + per-peer
   `PeerInfo { encrypt: bool, lmk: Option<[u8;16]> }` — but **no broadcast-encrypt knob**,
   because the blob has none.

**Cross-check ([#200 RIOT](../research/riot-espnow-study.md)):** RIOT hard-`#error`s *"if
esp_wifi is used, esp_now must be unicast"* — it mandates unicast **because** the broadcast
path can't carry encrypted semantics, and it sidesteps by never broadcasting. **smol is
flood-first and cannot sidestep** — so encryption of the mesh transport is off the table.

**Fork A — go unicast-mesh (encrypt each hop with LMK): SKIP.** It contradicts flood-first
(the #13/#123 architecture the 07-14 pathology + #200 both validate) *and* the encrypted-peer
cap (~7) is far below a 4–30 node fleet (below even #28's total-20). Not viable.

### 4.1 Rung B1 — the group HMAC-SHA256 (the reshaped #190)

**Mechanism.** Append a **truncated HMAC-SHA256(payload, GROUP_KEY)** to broadcast frames.
This is **authenticity in software** (sha256 already in-tree, `ota.rs`), not hardware
encryption. Verify-then-parse (the OTAM order, §7.3): a frame whose MAC doesn't match the
group key is **dropped before smol's parser sees it** — outsiders (attacker classes 1 & 2,
§1.3) are off the mesh for +8–16 B and one shared secret. It replaces exactly what the dead
AES-CCM rung was supposed to buy, minus confidentiality (§4.3).

**GROUP_KEY provisioning.** A 32-B HMAC key in `secrets.rs` (git-ignored), flash-provisioned
exactly like the WiFi password (and exactly where the PMK would have lived). Public-repo
hygiene unchanged: **never** in a committed file (the [smol public-repo topology] rule).

**GROUP_KEY rotation.** Carry a **1-byte key-epoch** in the MAC'd header. A transition release
accepts **both** epoch *N* and *N+1* (compute/verify against both keys for one release), then a
later release drops epoch *N* — the same two-key overlap the original PMK row needed, but now
**soft** (a wrong-epoch frame is dropped like any bad MAC, not a hardware-layer partition).
Rotate by OTA-ing the new key + epoch bump; no reflash required (unlike the dead AES-CCM path).

**Truncation length vs the 250 B budget — the tradeoff (spelled out).** Full HMAC-SHA256 is
32 B (13 % of MTU) — too much for high-rate frames. Truncate:
- **8 B (64-bit)** for high-rate frames (HELLO 2 Hz×N, TIME, FAM, RELAY/flood).
- **12–16 B (96–128-bit)** for medium-rate frames that want more margin.

**Security of truncation (why 64 bits is safe here).** A truncated MAC's only weakness is
*online forgery* — an attacker must transmit a frame and guess the tag; there is **no offline
attack** (the key is not recoverable from observed tags, and HMAC-SHA256's key strength is
unaffected by output truncation — truncation only shortens the tag, not the construction). At
64-bit, forgery probability is **2⁻⁶⁴ per attempt**; at ESP-NOW's on-air frame rate (~hundreds
/s ceiling) a single expected forgery is ~10¹¹ years. 96-bit (IPsec AH/ESP's choice) is
overkill margin. **So the tradeoff is pure airtime vs forgery-margin, and 8 B is ample for the
hobby threat model** — recommend 8 B default, 12 B for the medium-rate records that can spare it.

**What carries the MAC.** **Every broadcast frame** (the cheap outsider shield). The
catastrophic 3 (§1.2) carry the MAC **plus** a 64-B ed25519 signature (§4.2). Nuisance-tier
frames carry the MAC and nothing else — that is sufficient outsider defense for them (§8).

### 4.2 Rung B2 — how the MAC folds with #184 (compose, not compete)

The group MAC and ed25519 signing are **complementary layers on different threats**, so a
catastrophic frame carries both:

| | Group MAC (B1, reshaped #190) | ed25519 sig (#184) |
|---|---|---|
| Crypto | symmetric HMAC-SHA256 (group key) | asymmetric ed25519 (per-author key) |
| Defends | **outsider** (no group key) — classes 1 & 2 | **insider / key-leak / physical extraction** — class 3 |
| Cost | +8 B, every frame | +64 B, the 3 catastrophic records only |
| Rate | high-rate OK | low-rate only (election/command/arm) |
| Property | authenticity (any keyed board can forge *content*) | **non-repudiation** (only the private-key holder can author) |

**They compose, and the cheap one runs first:** on a catastrophic frame, verify the 8-B group
MAC → drop outsiders *before* spending the ~ms ed25519 verify; then verify the 64-B sig for
non-repudiable author proof. `HMAC-then-verify-sig`, both in front of `parse` (§7.3). So B1
is the cheap broad filter for **everything**; #184 is the expensive targeted proof for the
**3 records that can brick or hijack the fleet** — exactly the §3 tier split, now with the MAC
(not dead AES-CCM) as tier B. This is strictly cleaner than the original design, where AES-CCM
and ed25519 overlapped on the catastrophic records; now each rung owns a distinct threat.

### 4.3 Confidentiality — **SKIP** (recorded verdict + rationale)

The reshaped #190 provides **authenticity, not confidentiality**: broadcast payloads travel in
**plaintext** (an in-range sniffer can read HELLO/telemetry/game state). This is a deliberate
verdict, not an oversight:
- smol's threat model is **"trust your own co-located boards; keep outsiders off the wire +
  protect the catastrophic records."** There are **no secret payloads** — telemetry, time,
  game state, and the LAN topology are already accepted-public (the [smol public-repo topology]
  call). An eavesdropper learns nothing worth defending.
- True broadcast confidentiality would need a **bespoke app-layer AEAD** (group-keyed
  AES-GCM/ChaCha20 in software over *every* frame) — real CPU on the deaf-⅓ single radio + key
  + nonce management — unjustified for a hobby mesh with nothing secret to hide.

**SKIP confidentiality** unless the threat model later gains a genuine secrecy requirement; then
revisit an app-layer AEAD (a much bigger lift than the MAC).

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
| **GROUP_KEY** (#190, reshaped — was "PMK") | symmetric 32 B (HMAC-SHA256 key) | `secrets.rs` (git-ignored), fleet-shared | flash-time provisioning | **OTA-able** via a 1-byte key-epoch + two-epoch overlap window (accept epoch N & N+1 for one release, then drop N) — a wrong-epoch frame is *dropped like any bad MAC*, **not** a hardware partition, so rotation no longer needs a fleet reflash (§4.1) |
| **OTA/INTENT signing key** (#184 model A) | ed25519, private **off-device** | build host / operator only | pubkey hardcoded in firmware (`OTA_SIGNING_PUBKEY` precedent) | new pubkey = a firmware release; sign the transition announce with the *old* key |
| **Per-node key** (#184 model B, if adopted) | ed25519, private **on-device** | generated per board (first boot → NVS), pubkey gossiped/pinned | signed roster from the crown, or trust-on-first-use + pin | per-board reflash / re-gen and re-distribute pubkey |

**Rotation is the hard part and must not brick the fleet** — see §7. The safe pattern for every key: **overlap window** — accept both old and new for one release, so a half-rolled fleet still verifies.

---

## 7. Mixed-fleet rollout / compat (#100-class discipline — the brick-avoidance rung)

**The hard invariant: an old board must not brick or partition when a signed/encrypted record appears, and a new board must not reject an old board into isolation during the rollout.** smol's compat rule (#100/#56): **additive + ignore-unknown**, verified against real mixed-fleet.

### 7.1 #190 (reshaped) is now ADDITIVE too — the deafness risk is gone
> **AMENDED (Fork B).** The original rung was hardware AES-CCM — a **non-additive hard
> cutover** (an encrypted frame is dropped by the MAC layer on an unkeyed board → instant
> self-inflicted #204-class partition). That danger is **eliminated by the reshape:** the
> group MAC is an **appended field**, exactly like an ed25519 signature (§7.2), so an old
> board simply ignores the extra 8 B and still parses the frame. The transport rung joins the
> *safe, additive* category:
- **Observe → enforce** (same two-phase as §7.2 signing): a transition release **appends** the
  group MAC on TX and **soft-accepts** un-MAC'd frames on RX (old boards keep working); watch a
  `mac_fail` counter (mirror `verify_fail`, `ota_mesh.rs:481`); a later release **hard-drops**
  un-MAC'd broadcast (`verify-MAC-or-drop`) once the fleet is fully MAC'ing.
- **No hardware-layer deafness, no flag day, no partition window** — a wrong/absent MAC is a
  soft app-layer drop, reversible by config, never a MAC-layer blackout. Still **canary the
  enforce flip** (one board, positive inbound proof) out of #204 discipline — but the failure
  mode is now "drops some frames," not "goes deaf to the fleet."

### 7.2 #184 (signing) IS additive — the safe one
A signature is an **appended field**; an old board ignores the extra bytes (or the record is dual-published: unsigned for old, signed for new). Rollout:
- Old boards ignore the sig and accept the record on its existing (unauth) merits — **no brick, but also no new protection for them** (acceptable during transition).
- New boards **prefer** a valid sig; during transition they accept unsigned records too (soft-enforce), then a later release **hard-rejects** unsigned catastrophic records (`verify or drop`, like OTAM already does). Two-phase: **observe → enforce**, so a fleet that isn't fully signing yet doesn't lose its crown.
- The `verify_fail` counter (already in `ota_mesh.rs:481`) is the observability: watch it during the observe phase before flipping to enforce.

### 7.3 The composition trap (must verify)
Signing/encryption must not break the guards I and others just shipped: **#155 channel_hint gate, #146/#136/#137/#150 election guards, #204 self-heal.** A signed MC still has to flow through the same resolver; the group-MAC filter still has to let the #204 detector see "sustained inbound" (a MAC drop must not be mistaken for RF deafness — count MAC-fails separately). **Design so auth is a filter *in front of* the existing logic, never a rewrite of it** — verify-then-parse (the OTAM order), exactly as #184's verify gate already sits before `parse_manifest`.

**How a group-key mishap actually manifests (from morpheus-155's #204 review — shapes the canary).** The group MAC rides **ESP-NOW broadcast** (HELLO / flood / TIME / FAM), but #204's crown-liveness `got_mc` detector keys on the **MQTT retained downlink (TCP, not ESP-NOW)**. So a bad GROUP_KEY / wrong key-epoch does **NOT** trip a crown-deaf-shed — it makes **leaves drop the crown's HELLO → owner-lock lost → recovery-election churn**. Two consequences for the observe→enforce rollout (§7.1): (a) the transition canary must watch **leaf owner-lock stability**, not just crown liveness — HELLO-drop churn is the failure signature here; and (b) **never flip enforce during (or adjacent to) a #204 episode** — a real crown-loss + a MAC-enforce flip would compound into hard-to-diagnose churn. Gate the flip on a quiet fleet.

---

## 8. What NOT to sign / encrypt (C3 perf & RAM budget)

The C3 is single-core RISC-V @160 MHz, ~400 KB SRAM, no PSRAM. ed25519 verify is ~ms-scale; **signing** is comparable but adds RNG + key handling. Budget rules:

- **DON'T sign** high-rate/nuisance frames: HELLO (2 Hz × N boards), BEACON, RELAY telemetry, DIAG, STAT, SCAN, FAM. Signing these would (a) blow airtime on the deaf-⅓ single radio and (b) burn CPU on a board already CPU-blocking during flush. **The reshaped #190 group MAC covers them for +8 B + one sha256 (not a 64-B sig, not a ~ms verify); that's sufficient for nuisance-tier.** *(Amended: the original said "#190 covers them for free" when #190 = zero-cost hardware AES-CCM; the app-layer MAC is +8 B/frame + cheap sha256 — still far below a signature, but not literally free. Budget it.)*
- **DON'T add a *second* MAC** — post-reshape, the group MAC (§3 tier B) **IS** #190; every broadcast frame already carries it. Don't layer another symmetric MAC on top. *(Amended: the original bullet said "don't MAC if #190 is on — it duplicates the PMK" — self-contradictory now that the MAC is #190, not AES-CCM.)*
- **DO sign** only: OTA arm (done), crown MC, remote-reboot/config INTENT — all **low-rate** (per-election / per-command).
- **RAM:** verify is no-alloc (proven, `ota.rs`); a per-node signing key adds 32 B priv + 32 B pub in NVS — negligible. The 64-B sig buffers are stack, sized like the existing OTAM path.
- **Airtime is the real budget, not CPU:** the [smol retire-the-burst] deaf window means every extra broadcast byte matters. This is why §3's chain-of-trust (sign the root, not every record) is the scalable answer.

---

## 9. Recommended build order (JP decides)

A dependency-ordered menu, cheapest-highest-value first:

1. **#184 model A for INTENTs (no new crypto).** Sign remote-reboot/config commands off-device; boards verify with the existing `verify_signature`. Closes the reboot-brick injection with today's code + a publish-side signing step. **Cheapest catastrophic-record win.**
2. **Sign the #155 channel_hint / crown-steer (model A).** Make the operator lever cryptographically authenticated — small, composes with shipped #155.
3. **#190 (reshaped) — the group MAC:** ~~spike the broadcast-encryption API question~~ **DONE (#36): broadcast can't be encrypted, so #190 = the group HMAC (§4).** Implement the MAC at the frame codec, roll out **observe → enforce** (§7.1, additive/soft — no deafness risk). Still high-value (it IS the outsider shield for the catastrophic broadcast records), and no longer gated behind an unknown.
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

## 11. Open questions (for JP)

1. ~~**#190 broadcast encryption** — does `esp-wifi` support PMK-encrypted *broadcast*?~~ **✅ RESOLVED (#36): NO — unicast-LMK only.** #190 is reshaped to the group HMAC (§4); this is no longer an unknown. *The remaining #190 decision for JP is simply: greenlight the group-MAC rung, or defer it — there are no impossible options left in it.*
2. **On-device key gate (#184 model B)** — is autonomous self-election authenticity worth a per-board private key on physically-accessible hardware? Or is the model-A operator-signed-steer fallback (§5.3) enough for v1?
3. **Initial provisioning** — flag-day reflash vs a two-release dual-listen window (§7) for *first* GROUP_KEY rollout? *(Rotation itself is now OTA-able via the key-epoch overlap — §4.1/§6 — so this question is about first provisioning only, not ongoing rotation.)*
4. **Enforce threshold** — after the observe phase, what `verify_fail` rate / soak duration gates the flip from soft-accept to hard-reject-unsigned?
5. **Replay** — is `dl_seq` sufficient anti-replay for signed INTENTs, or does a reboot/command need a nonce + freshness window (a signed reboot captured off the air could be replayed later)?

---

*Design only. The one thing already true today: smol ed25519-verifies its most dangerous frame and fails closed. #184 is "do that for two more records"; #190 (reshaped, Fork B) is "add a cheap group MAC so outsiders are dropped at the door." Ship the cheap catastrophic-record wins (§9.1–2) first; #190's broadcast question is **answered** (#36 — no native encryption), so #190 is now a clean greenlight/defer on the group-MAC rung, not a spike.*
