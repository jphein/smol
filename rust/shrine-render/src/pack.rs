//! The SD voice pack's manifest (0033 "Speaking"): parse `MANIFEST.TSV` and find a clip by id.
//!
//! `tools/voice_pack.py` renders `game/voice/clips.tsv` with Piper into one directory per set
//! (`/TAPSTONE/VOICE/SET1/` on the card): 8.3-named IMA-ADPCM WAVs and this manifest. The format is
//! the builder's docstring; this module is the shrine's side of it, and
//! `tests/fixtures/voice-pack-manifest.tsv` is one file both sides test against (the Python suite
//! asserts the builder still writes it byte for byte).
//!
//! # Why here, and why this shape
//!
//! The shrine is the only reader, and this crate is what smol vendors for the shrine; the clip ids
//! are `shrine-preview`'s keys for [`crate::voice::Voice`] values. `tapstone-proto` is the wire
//! codec the arena links too, and the arena never reads the card.
//!
//! No allocator: [`Manifest`] borrows the manifest's text, and every [`Clip`] borrows from that.
//! [`Manifest::parse`] validates every row once (so a bad card is refused at mount, not at the
//! first line spoken), and [`Manifest::find`] is then a line scan that stops early because rows are
//! sorted by id. 280 rows is ~40 KB, which the S3 holds in PSRAM; a manifest read row by row from
//! the card would use [`parse_row`] the same way.
//!
//! The reader refuses any format but the one the shrine decodes, so a pack rendered for another
//! rate or codec cannot mount and play as noise.

/// The first line of every v1 manifest.
pub const VERSION: &str = "# tapstone voice pack v1";
/// The column header row.
pub const COLUMNS: &str = "id\tfile\tbytes\tsha256\ttext_sha256\ttext";
/// The only audio format a v1 shrine plays: IMA-ADPCM, 22,050 Hz (the codec's floor, BOARD.md L5),
/// 256-byte blocks.
pub const FORMAT: &str = "ima-adpcm\t22050\t256";

/// Why a manifest was refused. `line` counts from 1, as an editor would show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackError {
    /// The first line is not [`VERSION`].
    Version,
    /// A `#` line is missing or malformed: which one.
    Meta(&'static str),
    /// The pack is in a format the shrine does not decode.
    Format,
    /// No [`COLUMNS`] row after the meta lines.
    Columns,
    /// A clip row that does not parse.
    Row { line: usize },
    /// A row whose id is not strictly after the previous one (unsorted, or a duplicate).
    Unsorted { line: usize },
    /// `# clips` disagrees with the rows: rows counted, bytes summed.
    Count { rows: usize, bytes: u64 },
}

/// One clip: where its audio is and what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clip<'a> {
    pub id: &'a str,
    /// An 8.3 name in the set's directory, e.g. `5014F9AF.WAV`.
    pub file: &'a str,
    pub bytes: u32,
    /// sha256 of the file: what the shrine checks a clip against after reading it.
    pub sha256: [u8; 32],
    pub text_sha256: [u8; 32],
    /// The sentence, which the voice band shows whether or not the clip plays.
    pub text: &'a str,
}

/// A parsed, validated manifest, borrowing its text.
#[derive(Debug, Clone, Copy)]
pub struct Manifest<'a> {
    voice: &'a str,
    rows: &'a str,
    len: usize,
    bytes: u64,
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = nib(b[2 * i])? << 4 | nib(b[2 * i + 1])?;
    }
    Some(out)
}

fn is_8_3_wav(f: &str) -> bool {
    let b = f.as_bytes();
    b.len() == 12
        && &b[8..] == b".WAV"
        && b[..8]
            .iter()
            .all(|c| matches!(c, b'0'..=b'9' | b'A'..=b'F'))
}

/// Parse one clip row (no line ending). `None` if any field is malformed.
pub fn parse_row(row: &str) -> Option<Clip<'_>> {
    let mut f = row.split('\t');
    let (id, file, bytes, sha, tsha, text) = (
        f.next()?,
        f.next()?,
        f.next()?,
        f.next()?,
        f.next()?,
        f.next()?,
    );
    if f.next().is_some() || id.is_empty() || text.is_empty() || !is_8_3_wav(file) {
        return None;
    }
    if bytes.is_empty() || !bytes.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(Clip {
        id,
        file,
        bytes: bytes.parse().ok()?,
        sha256: hex32(sha)?,
        text_sha256: hex32(tsha)?,
        text,
    })
}

fn meta<'a>(line: Option<&'a str>, key: &'static str) -> Result<&'a str, PackError> {
    line.and_then(|l| l.strip_prefix("# "))
        .and_then(|l| l.strip_prefix(key))
        .and_then(|l| l.strip_prefix('\t'))
        .ok_or(PackError::Meta(key))
}

impl<'a> Manifest<'a> {
    /// Validate the whole manifest: version, format, every row, their order and the declared totals.
    pub fn parse(text: &'a str) -> Result<Self, PackError> {
        let mut lines = text.lines();
        if lines.next() != Some(VERSION) {
            return Err(PackError::Version);
        }
        let voice = meta(lines.next(), "voice")?;
        if meta(lines.next(), "format")? != FORMAT {
            return Err(PackError::Format);
        }
        let (n, total) = meta(lines.next(), "clips")?
            .split_once('\t')
            .ok_or(PackError::Meta("clips"))?;
        let (n, total): (usize, u64) = (
            n.parse().map_err(|_| PackError::Meta("clips"))?,
            total.parse().map_err(|_| PackError::Meta("clips"))?,
        );
        if lines.next() != Some(COLUMNS) {
            return Err(PackError::Columns);
        }
        // Everything after the column header, found by position so find() can scan it again.
        let header_end = text
            .find(COLUMNS)
            .map(|i| i + COLUMNS.len())
            .ok_or(PackError::Columns)?;
        let rows = text[header_end..].strip_prefix('\n').unwrap_or("");
        let (mut len, mut bytes, mut prev) = (0usize, 0u64, None::<&str>);
        for (i, row) in rows.lines().enumerate() {
            let line = 6 + i;
            let c = parse_row(row).ok_or(PackError::Row { line })?;
            if prev.is_some_and(|p| p >= c.id) {
                return Err(PackError::Unsorted { line });
            }
            prev = Some(c.id);
            len += 1;
            bytes += u64::from(c.bytes);
        }
        if len != n || bytes != total {
            return Err(PackError::Count { rows: len, bytes });
        }
        Ok(Self {
            voice,
            rows,
            len,
            bytes,
        })
    }

    /// The Piper voice the pack was rendered with.
    pub fn voice(&self) -> &'a str {
        self.voice
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The audio's total size, every file summed.
    pub fn total_bytes(&self) -> u64 {
        self.bytes
    }

    /// Every clip, in id order.
    pub fn clips(&self) -> impl Iterator<Item = Clip<'a>> + 'a {
        // parse() has already accepted every row, so none is dropped here.
        self.rows.lines().filter_map(parse_row)
    }

    /// The clip with this id. `None` means the pack has no audio for it: show the text, say nothing.
    pub fn find(&self, id: &str) -> Option<Clip<'a>> {
        for c in self.clips() {
            match c.id.cmp(id) {
                core::cmp::Ordering::Less => continue,
                core::cmp::Ordering::Equal => return Some(c),
                core::cmp::Ordering::Greater => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written by tools/voice_pack.py from its test clips; the Python suite pins it to the builder.
    const FIXTURE: &str = include_str!("../tests/fixtures/voice-pack-manifest.tsv");

    fn with_line(i: usize, new: &str) -> heapless::String<2048> {
        let mut s = heapless::String::new();
        for (n, l) in FIXTURE.lines().enumerate() {
            s.push_str(if n == i { new } else { l }).unwrap();
            s.push('\n').unwrap();
        }
        s
    }

    #[test]
    fn the_builders_manifest_parses() {
        let m = Manifest::parse(FIXTURE).unwrap();
        assert_eq!(m.voice(), "fake-voice");
        assert_eq!(m.len(), 3);
        assert_eq!(m.total_bytes(), 28340);
        let ids: heapless::Vec<&str, 8> = m.clips().map(|c| c.id).collect();
        assert_eq!(ids.as_slice(), ["invite", "level_up.2", "pass"]);
    }

    #[test]
    fn find_returns_every_clip_by_id_and_nothing_else() {
        let m = Manifest::parse(FIXTURE).unwrap();
        let c = m.find("pass").unwrap();
        assert_eq!(
            (c.file, c.bytes, c.text),
            ("D74FF0EE.WAV", 5180, "you pass")
        );
        assert_eq!(c.sha256[..4], [0xe5, 0x13, 0x37, 0xbc]);
        assert_eq!(
            m.find("invite").unwrap().text,
            "set your castle on the stone"
        );
        assert_eq!(m.find("level_up.2").unwrap().file, "21E78684.WAV");
        for missing in [
            "",
            "a",
            "invitf",
            "level_up",
            "level_up.3",
            "pas",
            "passes",
            "zzz",
        ] {
            assert_eq!(m.find(missing), None, "{missing}");
        }
    }

    #[test]
    fn another_format_is_refused() {
        let s = with_line(2, "# format\tima-adpcm\t16000\t256");
        assert_eq!(Manifest::parse(&s).unwrap_err(), PackError::Format);
        assert_eq!(
            Manifest::parse(&with_line(0, "# tapstone voice pack v2")).unwrap_err(),
            PackError::Version
        );
    }

    #[test]
    fn a_malformed_row_is_refused_with_its_line() {
        let row = FIXTURE.lines().nth(6).unwrap();
        let bad_hash = row.replacen("85bf", "85BF", 1); // uppercase hex is not what the builder writes
        assert_eq!(
            Manifest::parse(&with_line(6, &bad_hash)).unwrap_err(),
            PackError::Row { line: 7 }
        );
        let long_name = row.replacen("21E78684.WAV", "21E78684X.WAV", 1);
        assert_eq!(
            Manifest::parse(&with_line(6, &long_name)).unwrap_err(),
            PackError::Row { line: 7 }
        );
        let extra = with_line(6, &{
            let mut s = heapless::String::<256>::new();
            s.push_str(row).unwrap();
            s.push_str("\textra").unwrap();
            s
        });
        assert_eq!(
            Manifest::parse(&extra).unwrap_err(),
            PackError::Row { line: 7 }
        );
    }

    #[test]
    fn unsorted_or_duplicate_rows_are_refused() {
        let (a, b) = (
            FIXTURE.lines().nth(5).unwrap(),
            FIXTURE.lines().nth(6).unwrap(),
        );
        let swapped = with_line(5, b);
        let swapped = {
            let mut s = heapless::String::<2048>::new();
            for (n, l) in swapped.lines().enumerate() {
                s.push_str(if n == 6 { a } else { l }).unwrap();
                s.push('\n').unwrap();
            }
            s
        };
        assert_eq!(
            Manifest::parse(&swapped).unwrap_err(),
            PackError::Unsorted { line: 7 }
        );
        assert!(matches!(
            Manifest::parse(&with_line(6, a)).unwrap_err(),
            PackError::Unsorted { line: 7 }
        ));
    }

    #[test]
    fn the_declared_totals_must_match_the_rows() {
        let s = with_line(3, "# clips\t3\t28341");
        assert_eq!(
            Manifest::parse(&s).unwrap_err(),
            PackError::Count {
                rows: 3,
                bytes: 28340
            }
        );
        let mut short = heapless::String::<2048>::new();
        for l in FIXTURE.lines().take(7) {
            short.push_str(l).unwrap();
            short.push('\n').unwrap();
        }
        assert_eq!(
            Manifest::parse(&short).unwrap_err(),
            PackError::Count {
                rows: 2,
                bytes: 23160
            }
        );
    }

    /// The real pack, which lives outside git: `TAPSTONE_VOICE_PACK=<dir>/MANIFEST.TSV cargo test -p
    /// shrine-render -- --ignored real_pack`. Panics without the variable rather than passing blind.
    #[test]
    #[ignore]
    fn real_pack() {
        extern crate std;
        let p = std::env::var("TAPSTONE_VOICE_PACK")
            .expect("TAPSTONE_VOICE_PACK=<path to MANIFEST.TSV>");
        let text = std::fs::read_to_string(p).unwrap();
        let m = Manifest::parse(&text).unwrap();
        assert!(m.len() >= 280, "{}", m.len());
        for c in m.clips() {
            assert_eq!(m.find(c.id), Some(c));
        }
        std::println!(
            "real pack: {} clips, {} B, voice {}",
            m.len(),
            m.total_bytes(),
            m.voice()
        );
    }
}
