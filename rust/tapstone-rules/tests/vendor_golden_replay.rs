//! smol issue 10's smoke test, and the ONE test in this directory that smol wrote.
//!
//! Everything else under `tests/` is a byte-identical copy of tapstone's own suite (see
//! `Cargo.toml`); this file is smol's, and `tools/tapstone_vendor.sh` knows not to expect it
//! upstream.
//!
//! ── WHAT IT PROVES, AND WHY IT IS THE POINT OF THE WHOLE VENDORING STEP ────────────────────────
//! Tapstone's two shrines never exchange game state. They exchange TAP EVENTS and an 8-byte chain
//! head, and each shrine recomputes the state itself (decision 0004). That only works if the
//! engine on the shrine and the engine on the host are the same engine to the bit: one differing
//! byte in the canonical state image (134 B, `hash.rs`) makes two shrines compute different heads
//! from the same taps, and the table's answer to "who won" becomes "they disagree".
//!
//! So this replays a transcript the HOST engine produced — decks, house rules, every record, and
//! the expected chain head after each one — through the VENDORED engine, and demands the same
//! hashes. Not a smoke test in the "does it link" sense: it is the agreement proof the mesh
//! protocol rests on.
//!
//! ── THE FILE IS THE ORACLE. THERE IS NO HASH WRITTEN IN THIS SOURCE. ──────────────────────────
//! Every expected value is read from `testdata/seed-1.json`: `final_hash`, the per-record `hash`
//! field, `winner`, `rounds`, `game_over`, and `refusals`. That is deliberate and it is a scar.
//! smol issue 10 originally NAMED the expected hash in its own prose (`a01ea2b141f12056`) and went
//! stale inside a day — tapstone's sim changed which card its scripted seat picks, which changed
//! the transcript, which changed the hash, while the ENGINE did not move at all. A number copied
//! out of a data file into prose is a second source of truth that nothing updates.
//!
//! Vendoring the transcript instead of the number keeps that property: if the transcript is ever
//! re-vendored, the expectations travel with it in the same commit.
//!
//! ── WHY THE TRANSCRIPT IS A FROZEN VECTOR AND `src/` IS A PINNED MIRROR ───────────────────────
//! These two have DIFFERENT drift rules, and the difference is load-bearing:
//!
//!   `src/`                  must equal tapstone at tag `rules-v0.1.0`, forever, byte for byte.
//!                           Divergence means two engines. `--check` fails closed on it.
//!   `testdata/seed-1.json`  is a frozen TEST VECTOR: a fixed input with its fixed expected
//!                           output. It is not required to track tapstone's current golden,
//!                           because tapstone's goldens legitimately move whenever its sim's
//!                           scripted seats change — a sim-side event that says nothing about the
//!                           engine. Chasing it would manufacture exactly the staleness churn the
//!                           paragraph above is about.
//!
//! The vector was taken from tapstone `main` (not from the tag) so that the hash a reviewer reads
//! here is the hash `jq .final_hash` prints in tapstone today. It replays green through the tag's
//! engine because `src/` is byte-identical between the tag and that commit — verified, not assumed.
//! Engine divergence is still caught behaviourally, by `--check`'s third layer replaying tapstone's
//! CURRENT golden through this copy whenever the sibling repo is present.
//!
//! ── WHY THERE IS A JSON PARSER IN HERE AND NOT A `serde` DEV-DEPENDENCY ───────────────────────
//! `--locked` on the S3 build is one of issue 10's four criteria, and it is the one that proves no
//! new dependency entered the graph. A dev-dependency on serde/serde_json would put a 3-crate host
//! tree into this crate's resolution for the sake of a test — in a crate whose whole selling point
//! is that its bare-metal closure is `sha2` and nothing else. So: ~110 lines of recursive-descent
//! JSON below, over the subset this file uses.
//!
//! It is a real parser, not a regex over pretty-printed text, because a scraper keyed to
//! whitespace would break the moment the transcript were emitted compact, and would break by
//! silently reading the wrong field rather than by failing. Note also that the parser cannot fake
//! a pass: every number it extracts feeds either the engine's input or the SHA-256 chain, so a
//! misparse changes the computed hash and the test goes red. The oracle checks the parser.

use tapstone_rules::{Applied, Chain, Game, HouseRules, Kind, Record, Winner};

// ── a small JSON reader ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn get(&self, key: &str) -> &J {
        match self {
            J::Obj(kv) => kv
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key {key:?} missing")),
            _ => panic!("get({key:?}) on a non-object"),
        }
    }
    fn arr(&self) -> &[J] {
        match self {
            J::Arr(v) => v,
            _ => panic!("expected an array, got {self:?}"),
        }
    }
    fn int(&self) -> i64 {
        match self {
            J::Int(n) => *n,
            _ => panic!("expected an integer, got {self:?}"),
        }
    }
    fn str(&self) -> &str {
        match self {
            J::Str(s) => s,
            _ => panic!("expected a string, got {self:?}"),
        }
    }
    fn bool(&self) -> bool {
        match self {
            J::Bool(b) => *b,
            _ => panic!("expected a bool, got {self:?}"),
        }
    }
    /// `null` → `None`; anything else → `Some`. The per-record `hash` field is the only nullable
    /// one, and `null` there means "lobby record, before the chain's genesis".
    fn opt(&self) -> Option<&J> {
        match self {
            J::Null => None,
            other => Some(other),
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) {
        self.ws();
        assert_eq!(self.b.get(self.i), Some(&c), "expected {:?} at byte {}", c as char, self.i);
        self.i += 1;
    }
    fn peek(&mut self) -> u8 {
        self.ws();
        *self.b.get(self.i).expect("unexpected end of JSON")
    }
    fn lit(&mut self, s: &str) {
        assert!(self.b[self.i..].starts_with(s.as_bytes()), "expected {s:?} at byte {}", self.i);
        self.i += s.len();
    }
    fn value(&mut self) -> J {
        match self.peek() {
            b'{' => {
                self.eat(b'{');
                let mut kv = Vec::new();
                if self.peek() == b'}' {
                    self.eat(b'}');
                    return J::Obj(kv);
                }
                loop {
                    let k = match self.value() {
                        J::Str(s) => s,
                        other => panic!("object key must be a string, got {other:?}"),
                    };
                    self.eat(b':');
                    kv.push((k, self.value()));
                    if self.peek() == b',' {
                        self.eat(b',');
                    } else {
                        self.eat(b'}');
                        return J::Obj(kv);
                    }
                }
            }
            b'[' => {
                self.eat(b'[');
                let mut v = Vec::new();
                if self.peek() == b']' {
                    self.eat(b']');
                    return J::Arr(v);
                }
                loop {
                    v.push(self.value());
                    if self.peek() == b',' {
                        self.eat(b',');
                    } else {
                        self.eat(b']');
                        return J::Arr(v);
                    }
                }
            }
            b'"' => {
                self.eat(b'"');
                let start = self.i;
                // The transcript's strings are hex digits, `seatN`/`draw`, and CamelCase kind and
                // `applied` tokens — no escapes anywhere. Refuse one rather than mis-read it.
                while self.b[self.i] != b'"' {
                    assert_ne!(self.b[self.i], b'\\', "escapes are not supported (byte {})", self.i);
                    self.i += 1;
                }
                let s = core::str::from_utf8(&self.b[start..self.i]).expect("utf-8").to_string();
                self.i += 1;
                J::Str(s)
            }
            b't' => {
                self.lit("true");
                J::Bool(true)
            }
            b'f' => {
                self.lit("false");
                J::Bool(false)
            }
            b'n' => {
                self.lit("null");
                J::Null
            }
            _ => {
                let start = self.i;
                if self.b[self.i] == b'-' {
                    self.i += 1;
                }
                while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                let s = core::str::from_utf8(&self.b[start..self.i]).expect("utf-8");
                // INTEGERS ONLY, on purpose. Every number in a transcript is one — a seq, a seat,
                // a card design, a lane, a millisecond stamp. A float appearing here would mean the
                // schema changed under us, and the loud way to learn that is a panic, not an `as`
                // cast that silently truncates a value on its way into a hashed byte.
                J::Int(s.parse().unwrap_or_else(|_| panic!("not an integer: {s:?} at byte {start}")))
            }
        }
    }
}

fn parse(src: &str) -> J {
    let mut p = P { b: src.as_bytes(), i: 0 };
    let v = p.value();
    p.ws();
    assert_eq!(p.i, p.b.len(), "trailing bytes after the top-level value");
    v
}

// ── transcript → engine input ─────────────────────────────────────────────────────────────────

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Mirrors `tapstone-sim`'s `winner_str` exactly — the transcript's own spelling.
fn winner_str(w: Option<Winner>) -> Option<String> {
    w.map(|w| match w {
        Winner::Seat(0) => "seat0".to_string(),
        Winner::Seat(_) => "seat1".to_string(),
        Winner::Draw => "draw".to_string(),
    })
}

fn house_rules(j: &J) -> HouseRules {
    let g = |k: &str| {
        let n = j.get(k).int();
        u8::try_from(n).unwrap_or_else(|_| panic!("house rule {k} = {n} does not fit a u8"))
    };
    HouseRules {
        deck_size: g("deck_size"),
        hand: g("hand"),
        second_player_bonus: g("second_player_bonus"),
        castle_life: g("castle_life"),
        pressure_from: g("pressure_from"),
        pressure: g("pressure"),
        stop_round: g("stop_round"),
    }
}

fn kind(s: &str) -> Kind {
    match s {
        "ClaimSeat" => Kind::ClaimSeat,
        "Mulligan" => Kind::Mulligan,
        "Charge" => Kind::Charge,
        "CastUnit" => Kind::CastUnit,
        "CastSpell" => Kind::CastSpell,
        "Advance" => Kind::Advance,
        "Pass" => Kind::Pass,
        "Leave" => Kind::Leave,
        other => panic!("unknown record kind {other:?}"),
    }
}

fn uid(s: &str) -> [u8; 7] {
    assert_eq!(s.len(), 14, "uid {s:?} is not 14 hex digits");
    let mut out = [0u8; 7];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("uid hex");
    }
    out
}

fn record(j: &J) -> Record {
    Record {
        seq: u16::try_from(j.get("seq").int()).expect("seq fits u16"),
        seat: u8::try_from(j.get("seat").int()).expect("seat fits u8"),
        kind: kind(j.get("kind").str()),
        card: u16::try_from(j.get("card").int()).expect("card fits u16"),
        lane: i8::try_from(j.get("lane").int()).expect("lane fits i8"),
        target: u8::try_from(j.get("target").int()).expect("target fits u8"),
        aux: u8::try_from(j.get("aux").int()).expect("aux fits u8"),
        time_ms: u32::try_from(j.get("time_ms").int()).expect("time_ms fits u32"),
        uid: uid(j.get("uid").str()),
        auth: u8::try_from(j.get("auth").int()).expect("auth fits u8"),
    }
}

/// The castle designs, derived from the transcript rather than hardcoded.
///
/// They are not a top-level field, which looks at first like a hole in the oracle — it is not.
/// `ClaimSeat`'s `card` IS the castle design (`rules.rs::claim_seat` assigns
/// `seat.castle_design = r.card`), and `Game::new`'s castles are documented as "defaults until a
/// `ClaimSeat` replaces them". So the file does carry them, one level down, and reading them from
/// there keeps the whole input file-sourced. `tapstone-sim` passes its `CASTLES` const to
/// `Game::new` for the same values; deriving is equivalent AND survives a set where they differ.
fn castles(records: &[J]) -> [u16; 2] {
    let mut out = [None, None];
    for r in records {
        if r.get("kind").str() == "ClaimSeat" {
            let seat = usize::try_from(r.get("seat").int()).expect("seat index");
            assert!(seat < 2, "seat {seat} out of range");
            let design = u16::try_from(r.get("card").int()).expect("castle design fits u16");
            assert!(out[seat].is_none(), "seat {seat} claims twice");
            out[seat] = Some(design);
        }
    }
    [
        out[0].expect("no ClaimSeat for seat 0"),
        out[1].expect("no ClaimSeat for seat 1"),
    ]
}

// ── the test ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn vendored_engine_reproduces_the_hosts_transcript() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/seed-1.json");
    let src = std::fs::read_to_string(path).expect("the vendored transcript is missing");
    let t = parse(&src);

    let rules = house_rules(t.get("house_rules"));
    let decks: Vec<Vec<u16>> = t
        .get("decks")
        .arr()
        .iter()
        .map(|d| {
            d.arr()
                .iter()
                .map(|c| u16::try_from(c.int()).expect("card design fits u16"))
                .collect()
        })
        .collect();
    assert_eq!(decks.len(), 2, "a Duel is two seats");
    let records = t.get("records").arr();
    assert!(!records.is_empty(), "an empty transcript proves nothing");

    // Exactly `tapstone-sim`'s `replay()`: a fresh Game, a fresh Chain taken at `Started`, every
    // record applied in order. Held deliberately close to that function — this test is only
    // evidence of agreement if it is the same walk over the same input.
    let mut g = Game::new(rules, castles(records), [&decks[0], &decks[1]]);
    let mut chain: Option<Chain> = None;

    for rj in records {
        let r = record(rj);
        // A transcript records only ACCEPTED events, so a refusal on replay is not a tolerable
        // difference — it is the two engines disagreeing about what is LEGAL, which is one of the
        // two ways they can diverge (the other being the state image, caught by the hash below).
        //
        // NOTE the transcript's own top-level `refusals` field is a different quantity and is
        // deliberately NOT compared here: it counts taps the sim's scripted seat ATTEMPTED and the
        // arbiter rejected, which are by definition absent from `records`. Asserting it against a
        // replay's refusal count would compare two unrelated numbers and pass on seed-1 only
        // because that game happens to have zero of the first kind.
        let applied: Applied = g.apply(&r).unwrap_or_else(|refusal| {
            panic!(
                "record {} ({:?}) was accepted by the host engine but REFUSED here: {refusal:?}\n\
                 That is engine divergence, not a bad transcript. Check src/ against tapstone at \
                 tag rules-v0.1.0 with tools/tapstone_vendor.sh --check.",
                r.seq, r.kind,
            )
        });

        // Genesis is taken when the game leaves the Lobby (hash.rs's stated contract): lobby
        // records are transcript-only and carry `hash: null`.
        let got: Option<String> = if applied == Applied::Started {
            chain = Some(Chain::genesis(&g.rules));
            None
        } else if let Some(c) = chain.as_mut() {
            c.step(&r, &g);
            Some(hex(&c.head()))
        } else {
            None
        };

        let want = rj.get("hash").opt().map(|h| h.str().to_string());
        assert_eq!(
            got, want,
            "chain head diverged at record {} ({:?}) — the state images differ from here on",
            r.seq, r.kind
        );
    }

    let final_hash = chain.map(|c| hex(&c.head())).unwrap_or_default();
    assert_eq!(
        final_hash,
        t.get("final_hash").str(),
        "final chain head disagrees with the transcript"
    );
    assert_eq!(
        winner_str(g.winner).as_deref(),
        t.get("winner").opt().map(|w| w.str()),
        "winner disagrees"
    );
    assert_eq!(
        i64::from(g.round),
        t.get("rounds").int(),
        "round count disagrees"
    );
    assert_eq!(
        g.phase == tapstone_rules::Phase::Over,
        t.get("game_over").bool(),
        "game_over disagrees"
    );
}

/// The transcript has to be a real game, not a stub that happens to hash consistently. Cheap, and
/// it is the guard against a future re-vendor quietly shipping a two-record file that passes
/// everything above.
#[test]
fn the_vendored_transcript_is_a_finished_game() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/seed-1.json");
    let t = parse(&std::fs::read_to_string(path).expect("transcript"));
    assert!(t.get("game_over").bool(), "the vector must be a finished game");
    assert!(t.get("records").arr().len() > 20, "too short to exercise combat");
    assert_eq!(t.get("final_hash").str().len(), 16, "an 8-byte head is 16 hex digits");
}
