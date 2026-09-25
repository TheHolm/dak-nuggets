//! Recovering wall-clock times from opencode identifiers.
//!
//! opencode IDs look like `ses_f2948c7fdffe1r7f03mBfBRLsy`: a type prefix, an
//! underscore, 12 hex characters, then 14 random characters. The hex part holds
//! the **low 48 bits** of `(unix_millis * 4096 + per_millisecond_counter)`.
//!
//! There are two variants:
//!
//! * [`Encoding::Ascending`] stores that value directly, so IDs sort oldest-first;
//! * [`Encoding::Descending`] stores its bitwise complement, so IDs sort
//!   newest-first.
//!
//! The variant is **not** discoverable from the ID itself, and an earlier attempt
//! here to guess it by decoding both ways and keeping whichever looked plausible
//! was wrong: near a wrap boundary the incorrect interpretation can also land in a
//! believable window. So the encoding is supplied by the caller, or looked up from
//! the prefix in [`Encoding::for_prefix`] - and an unknown prefix yields nothing
//! rather than a guess.
//!
//! Only 36 bits of the millisecond timestamp survive (48 bits of payload minus 12
//! counter bits), so the value wraps roughly every 795 days. The high bits are
//! reconstructed from the caller-supplied current time.
//!
//! All of this was reverse-engineered from the opencode binary and then checked
//! against real IDs whose `time.created` was known - see NOTES.md. Note that
//! opencode's own decoder assumes ascending and ignores the wrap, so it cannot
//! simply be copied.

/// Number of bits of payload carried by the hex portion of an ID.
const PAYLOAD_BITS: u32 = 48;
/// Bits reserved for the within-millisecond counter.
const COUNTER_BITS: u32 = 12;
/// Bits of millisecond timestamp that survive encoding.
const TIME_BITS: u32 = PAYLOAD_BITS - COUNTER_BITS;

/// Largest value the payload can hold; also the complement mask.
const PAYLOAD_MASK: u64 = (1u64 << PAYLOAD_BITS) - 1;
/// Mask selecting the surviving low bits of the millisecond timestamp.
const TIME_MASK: i64 = (1i64 << TIME_BITS) - 1;
/// One full wrap of the truncated timestamp, in milliseconds (~795 days).
const WRAP_MS: i64 = 1i64 << TIME_BITS;

/// How far in the past a decoded timestamp may be and still be believed.
const MAX_AGE_MS: i64 = 180 * 24 * 60 * 60 * 1000;

/// How far into the future a decoded timestamp may be and still be believed.
///
/// Not zero, because an ID is minted a moment before the timestamp the server
/// records alongside it, and because clocks are imperfect.
const MAX_SKEW_MS: i64 = 60 * 1000;

/// Which of opencode's two ID orderings an identifier uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Payload stored directly; IDs sort oldest-first.
    Ascending,
    /// Payload stored complemented; IDs sort newest-first.
    Descending,
}

impl Encoding {
    /// The encoding used by a known ID prefix.
    ///
    /// Only prefixes whose ordering has actually been established are listed:
    ///
    /// * `per` from opencode's source, whose constructor calls the ascending
    ///   generator; `que` is grouped with it because pending questions and
    ///   permissions come from the same request machinery.
    /// * `ses` and `msg` from decoding real IDs against the `time.created` the
    ///   server recorded for them - and they differ, which is exactly why this
    ///   table exists.
    ///
    /// Anything else returns `None`, so an unrecognised prefix produces no
    /// timestamp instead of a plausible-looking fiction.
    pub fn for_prefix(prefix: &str) -> Option<Encoding> {
        match prefix {
            "per" | "que" | "msg" => Some(Encoding::Ascending),
            "ses" => Some(Encoding::Descending),
            _ => None,
        }
    }
}

/// Splits an ID into its prefix and the 12 hex characters following the first `_`.
///
/// Returns `None` if the ID is not shaped like an opencode identifier, which
/// includes prefix-only and truncated strings.
fn split_id(id: &str) -> Option<(&str, &str)> {
    let underscore = id.find('_')?;
    let prefix = &id[..underscore];
    let rest = id.get(underscore + 1..)?;
    let hex = rest.get(..12)?;
    if prefix.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((prefix, hex))
}

/// Rebuilds a full millisecond timestamp from its surviving low bits.
///
/// Takes the high bits from `now_ms`, then steps back one wrap if that would place
/// the result implausibly in the future - which is what happens for an ID minted
/// just before the counter last wrapped.
fn unwrap_time(low: i64, now_ms: i64) -> i64 {
    let candidate = (now_ms & !TIME_MASK) | low;
    if candidate > now_ms + MAX_SKEW_MS {
        candidate - WRAP_MS
    } else {
        candidate
    }
}

/// True if a reconstructed timestamp is close enough to now to be believed.
fn plausible(ms: i64, now_ms: i64) -> bool {
    ms <= now_ms + MAX_SKEW_MS && ms >= now_ms - MAX_AGE_MS
}

/// Decodes the creation time of an opencode ID, in Unix milliseconds.
///
/// `now_ms` supplies the current time, used to rebuild the truncated high bits and
/// to sanity-check the result. Returns `None` when the ID is malformed or the
/// decoded time is not believable, so callers degrade to "unknown" rather than
/// displaying a wrong age.
pub fn timestamp_ms(id: &str, now_ms: i64, encoding: Encoding) -> Option<i64> {
    let (_, hex) = split_id(id)?;
    let raw = u64::from_str_radix(hex, 16).ok()?;

    let payload = match encoding {
        Encoding::Ascending => raw,
        Encoding::Descending => PAYLOAD_MASK - raw,
    };

    let low = (payload >> COUNTER_BITS) as i64 & TIME_MASK;
    let ms = unwrap_time(low, now_ms);
    plausible(ms, now_ms).then_some(ms)
}

/// Decodes an ID, choosing the encoding from its prefix.
///
/// Returns `None` for a prefix whose ordering is not known.
pub fn timestamp_ms_auto(id: &str, now_ms: i64) -> Option<i64> {
    let (prefix, _) = split_id(id)?;
    let encoding = Encoding::for_prefix(prefix)?;
    timestamp_ms(id, now_ms, encoding)
}

/// Returns the oldest decodable timestamp among `ids`.
///
/// Used for "how long has this been waiting", where the oldest pending request is
/// the one that answers the question. IDs that cannot be decoded are skipped
/// rather than poisoning the result.
pub fn oldest_timestamp_ms<'a, I>(ids: I, now_ms: i64) -> Option<i64>
where
    I: IntoIterator<Item = &'a str>,
{
    ids.into_iter()
        .filter_map(|id| timestamp_ms_auto(id, now_ms))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real session ID captured from opencode 1.18.32, together with the
    /// `time.created` the server reported for it. `ses_` is descending, and this
    /// ID was minted 1 ms before the timestamp was recorded.
    const REAL_SES_A: (&str, i64) = ("ses_f2948c7fdffe1r7f03mBfBRLsy", 1790308726787);
    /// A second real sample, where ID and `time.created` agree exactly.
    const REAL_SES_B: (&str, i64) = ("ses_f29457234ffejFzn9L4UNG7Hz7", 1790308945355);
    /// A real assistant message ID with its recorded `time.created`. Unlike
    /// sessions, `msg_` is ascending - the reason the prefix table exists.
    const REAL_MSG: (&str, i64) = ("msg_0d6c75ea3001xwhRotcmNTvdqj", 1790309785251);

    /// Builds an ID the way opencode does, for exercising both encodings.
    fn make_id(prefix: &str, ms: i64, counter: u64, encoding: Encoding) -> String {
        let value = ((ms as u64) * 4096 + counter) & PAYLOAD_MASK;
        let payload = match encoding {
            Encoding::Ascending => value,
            Encoding::Descending => PAYLOAD_MASK - value,
        };
        format!("{prefix}_{payload:012x}RANDOMRANDOM4")
    }

    /// The decoder reproduces the real recorded time of a live descending ID.
    #[test]
    fn decodes_real_descending_session_id() {
        let (id, created) = REAL_SES_A;
        let got = timestamp_ms(id, created, Encoding::Descending).expect("should decode");
        // Within a millisecond: the ID predates the recorded timestamp slightly.
        assert!((created - got).abs() <= 1, "got {got}, expected ~{created}");
    }

    /// A second live sample decodes exactly, confirming the scheme rather than a
    /// coincidence.
    #[test]
    fn decodes_second_real_descending_session_id() {
        let (id, created) = REAL_SES_B;
        assert_eq!(timestamp_ms(id, created, Encoding::Descending), Some(created));
    }

    /// The prefix table routes real session IDs to the descending decoder.
    #[test]
    fn auto_decodes_real_session_ids_via_prefix() {
        for (id, created) in [REAL_SES_A, REAL_SES_B] {
            let got = timestamp_ms_auto(id, created).expect("should decode");
            assert!((created - got).abs() <= 1, "{id}: got {got}, expected ~{created}");
        }
    }

    /// Ascending IDs - the variant questions and permissions use - decode too.
    #[test]
    fn decodes_ascending_id() {
        let now = 1790308945355;
        let id = make_id("per", now - 5_000, 1, Encoding::Ascending);
        assert_eq!(timestamp_ms(&id, now, Encoding::Ascending), Some(now - 5_000));
        assert_eq!(timestamp_ms_auto(&id, now), Some(now - 5_000));
    }

    /// Round-trips both encodings across a spread of ages.
    #[test]
    fn round_trips_both_encodings() {
        let now = 1790308945355;
        for age_ms in [0, 1, 999, 60_000, 86_400_000, 30 * 86_400_000] {
            for encoding in [Encoding::Ascending, Encoding::Descending] {
                let id = make_id("que", now - age_ms, 7, encoding);
                assert_eq!(
                    timestamp_ms(&id, now, encoding),
                    Some(now - age_ms),
                    "age {age_ms}, {encoding:?}"
                );
            }
        }
    }

    /// The two encodings are genuinely different, so using the wrong one gives
    /// either nothing or a different answer - never silently the right one.
    #[test]
    fn wrong_encoding_does_not_silently_agree() {
        let now = 1790308945355;
        let id = make_id("per", now - 5_000, 1, Encoding::Ascending);
        assert_ne!(
            timestamp_ms(&id, now, Encoding::Descending),
            Some(now - 5_000)
        );
    }

    /// Reconstruction works when the truncated counter has wrapped between the ID
    /// being minted and now. This is the case the old guessing heuristic got wrong.
    #[test]
    fn handles_wrap_boundary() {
        for encoding in [Encoding::Ascending, Encoding::Descending] {
            // Put "now" just after a wrap, and the ID just before it.
            let now = WRAP_MS * 3 + 5_000;
            let created = now - 10_000;
            let id = make_id("ses", created, 1, encoding);
            assert_eq!(timestamp_ms(&id, now, encoding), Some(created), "{encoding:?}");
        }
    }

    /// Prefixes whose ordering has been established are mapped; others are not.
    #[test]
    fn maps_known_prefixes_only() {
        assert_eq!(Encoding::for_prefix("per"), Some(Encoding::Ascending));
        assert_eq!(Encoding::for_prefix("que"), Some(Encoding::Ascending));
        assert_eq!(Encoding::for_prefix("msg"), Some(Encoding::Ascending));
        assert_eq!(Encoding::for_prefix("ses"), Some(Encoding::Descending));
        for unknown in ["prt", "evt", "job", "pty", "tool", "wrk", "", "xyz"] {
            assert_eq!(Encoding::for_prefix(unknown), None, "{unknown}");
        }
    }

    /// A real message ID decodes exactly, and via the opposite encoding to a
    /// session ID - confirming the two variants really do coexist.
    #[test]
    fn decodes_real_ascending_message_id() {
        let (id, created) = REAL_MSG;
        assert_eq!(timestamp_ms(id, created, Encoding::Ascending), Some(created));
        assert_eq!(timestamp_ms_auto(id, created), Some(created));
        // Decoding it the other way must not coincidentally agree.
        assert_ne!(timestamp_ms(id, created, Encoding::Descending), Some(created));
    }

    /// An unknown prefix decodes to nothing rather than being guessed at.
    #[test]
    fn auto_refuses_unknown_prefixes() {
        let now = 1790308945355;
        let id = make_id("prt", now - 1_000, 1, Encoding::Ascending);
        assert_eq!(timestamp_ms_auto(&id, now), None);
        // The same ID decodes fine when the caller states the encoding.
        assert_eq!(timestamp_ms(&id, now, Encoding::Ascending), Some(now - 1_000));
    }

    /// Malformed input is rejected rather than producing a bogus time.
    #[test]
    fn rejects_malformed_ids() {
        let now = 1790308945355;
        for bad in [
            "",
            "ses",
            "ses_",
            "ses_short",
            "nounderscore",
            "ses_zzzzzzzzzzzz", // right length, not hex
            "ses_f2948c7fdff",  // 11 hex digits, one short
            "_f2948c7fdffe",    // no prefix
        ] {
            assert_eq!(
                timestamp_ms(bad, now, Encoding::Ascending),
                None,
                "should reject {bad:?}"
            );
            assert_eq!(timestamp_ms_auto(bad, now), None, "should reject {bad:?}");
        }
    }

    /// An ID whose decoded time is not plausible yields nothing, so the caller can
    /// show "unknown" instead of a wrong duration.
    #[test]
    fn rejects_implausible_times() {
        let now = 1790308945355;
        let ancient = make_id(
            "ses",
            now - (MAX_AGE_MS + 86_400_000),
            1,
            Encoding::Descending,
        );
        assert_eq!(timestamp_ms(&ancient, now, Encoding::Descending), None);
    }

    /// A time slightly in the future is tolerated, since IDs are minted just
    /// before the server records a timestamp and clocks drift.
    #[test]
    fn tolerates_small_clock_skew() {
        let now = 1790308945355;
        let id = make_id("per", now + 500, 1, Encoding::Ascending);
        assert_eq!(timestamp_ms(&id, now, Encoding::Ascending), Some(now + 500));
    }

    /// The within-millisecond counter does not disturb the recovered time.
    #[test]
    fn counter_does_not_affect_decoded_time() {
        let now = 1790308945355;
        for counter in [1, 2, 17, 4095] {
            let id = make_id("per", now - 1_000, counter, Encoding::Ascending);
            assert_eq!(
                timestamp_ms(&id, now, Encoding::Ascending),
                Some(now - 1_000),
                "counter {counter}"
            );
        }
    }

    /// The oldest of several pending requests is the one reported.
    #[test]
    fn picks_oldest_timestamp() {
        let now = 1790308945355;
        let a = make_id("que", now - 1_000, 1, Encoding::Ascending);
        let b = make_id("que", now - 90_000, 1, Encoding::Ascending);
        let c = make_id("que", now - 30_000, 1, Encoding::Ascending);
        let ids = [a.as_str(), b.as_str(), c.as_str()];
        assert_eq!(oldest_timestamp_ms(ids, now), Some(now - 90_000));
    }

    /// Undecodable entries are skipped rather than discarding the whole set.
    #[test]
    fn skips_undecodable_when_picking_oldest() {
        let now = 1790308945355;
        let good = make_id("que", now - 4_000, 1, Encoding::Ascending);
        let ids = ["garbage", good.as_str(), "que_zzzzzzzzzzzz"];
        assert_eq!(oldest_timestamp_ms(ids, now), Some(now - 4_000));
    }

    /// An empty set has no oldest member.
    #[test]
    fn no_oldest_for_empty_input() {
        assert_eq!(oldest_timestamp_ms(Vec::<&str>::new(), 1790308945355), None);
    }
}
