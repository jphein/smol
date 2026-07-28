//! Print the id -> name map. **This replaces `scratch/board-names.md`**, which was a hand-maintained
//! table — the same shape of artefact as the hand-copied word corpora this change removed, and it
//! would have gone stale the moment the fleet renamed.
//!
//! A map you can regenerate in a second is strictly better than a map you have to remember to edit:
//! this reads the SAME vendored sigil corpus the firmware compiles in, so it cannot disagree with
//! what a board actually calls itself.
//!
//!   cargo run -p mesh-model --example board_names            # the live fleet
//!   cargo run -p mesh-model --example board_names -- --all    # all 256 ids
//!   cargo run -p mesh-model --example board_names -- 5 8 122  # specific ids
//!
//! `tools/board_names.sh` is the convenience wrapper.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let all = args.iter().any(|a| a == "--all");

    // The ids that have existed on the air. Not authoritative — a bench note, deliberately small.
    // Sourced from docs/BUILDING.md's provisioning history; `--all` needs no such list.
    const LIVE: &[(u8, &str)] = &[
        (5, "OLED bench board"),
        (8, "OLED bench board"),
        (50, "#198 Phase-2 measurement board (was id7)"),
        (51, "#198 Phase-2 measurement board (was id9)"),
        (122, "C6 watch — esp32c6-watch firmware, NOT this repo"),
        (236, "C6 watch — esp32c6-watch firmware, NOT this repo"),
    ];

    let explicit: Vec<u8> = args.iter().filter_map(|a| a.parse().ok()).collect();

    println!("{:<5} {:<20} {:<12} {}", "id", "sigil", "short(6)", "note");
    println!("{}", "-".repeat(78));

    let rows: Vec<(u8, &str)> = if all {
        (0..=255u8).map(|i| (i, "")).collect()
    } else if !explicit.is_empty() {
        explicit.iter().map(|&i| (i, "")).collect()
    } else {
        LIVE.to_vec()
    };

    for (id, note) in rows {
        let sigil = mesh_model::names::sigil_for_id(id);
        // Mirror the firmware's cramped-display label so a bench operator reads the same string the
        // OLED shows: noun clipped to the remaining room, then the id.
        let noun = mesh_model::names::noun_for_id(id);
        let digits = if id >= 100 { 3 } else if id >= 10 { 2 } else { 1 };
        let room = 6usize.saturating_sub(digits);
        let short = format!("{}{}", &noun[..room.min(noun.len())], id);
        let note = if note.is_empty() {
            LIVE.iter().find(|(l, _)| *l == id).map(|(_, n)| *n).unwrap_or("")
        } else {
            note
        };
        println!("{:<5} {:<20} {:<12} {}", id, sigil, short, note);
    }

    if !all {
        println!("\n(--all for every id; the sigil is a pure function of the id, so this needs no fleet contact)");
    }
}
