//! Wave-3 property/fuzz tests for `SampleFormat` string parsing and `Display`.
//!
//! Two families of properties:
//!
//! 1. NO-PANIC: [`SampleFormat::from_str`] is fed arbitrary strings (empty,
//!    unicode, `"a:b:c"`, huge/negative numbers, extra colons, wildcards).
//!    It must return `Ok` or `Err` — never panic, overflow, or abort.
//!
//! 2. ROUND-TRIP: for generated valid `(rate, bits, channels)`, formatting a
//!    `SampleFormat` to a string and parsing it back yields an equal value,
//!    and a hand-built `"r:b:c"` string parses to the expected accessors.
//!
use std::str::FromStr;

use proptest::prelude::*;
use snapcast_proto::SampleFormat;
use snapcast_proto::sample_format::SampleFormatError;

/// Strategy producing "interesting" field tokens for a `rate:bits:channels`
/// string: valid numbers, the `*` wildcard, empty, whitespace, signs, huge
/// values that overflow `u32`, hex-ish junk, and unicode.
fn arb_field_token() -> impl Strategy<Value = String> {
    prop_oneof![
        // Plain decimal numbers (incl. leading zeros, in and out of u32 range).
        "[0-9]{0,12}",
        // Wildcard sentinel.
        Just("*".to_string()),
        // Signed numbers (negatives are rejected by u32 parse).
        "[+-]?[0-9]{0,10}",
        // Whitespace-padded numbers (rejected: parse does not trim).
        " *[0-9]{0,6} *",
        // Non-numeric junk.
        "[a-zA-Z]{0,6}",
        // Values guaranteed to overflow u32.
        Just("4294967296".to_string()),
        Just("99999999999999999999".to_string()),
        // Values > u16::MAX but <= u32::MAX (u32 parse OK, then rejected by
        // checked conversion to u16).
        Just("70000".to_string()),
        Just("65536".to_string()),
        // Unicode / non-ASCII digits and symbols.
        "\\PC{0,4}",
    ]
}

/// Assemble an arbitrary colon-joined string from 0..=5 field tokens, so we
/// exercise "too few" (0,1,2), "just right" (3), and "too many" (4,5) parts.
fn arb_colon_string() -> impl Strategy<Value = String> {
    prop::collection::vec(arb_field_token(), 0..=5).prop_map(|parts| parts.join(":"))
}

proptest! {
    /// NO-PANIC over fully arbitrary UTF-8 strings, including empty, unicode,
    /// embedded colons, and control characters. Any `Result` is acceptable;
    /// panicking / overflowing is not.
    #[test]
    fn from_str_arbitrary_never_panics(s in ".*") {
        let _ = SampleFormat::from_str(&s);
    }

    /// NO-PANIC over structured colon-joined strings that stress the exact
    /// split/parse path (wrong part counts, `*`, huge numbers, negatives,
    /// unicode tokens).
    #[test]
    fn from_str_colon_shaped_never_panics(s in arb_colon_string()) {
        // Both the free function and the `.parse()` sugar must not panic.
        let _ = SampleFormat::from_str(&s);
        let _ = s.parse::<SampleFormat>();
    }

    /// NO-PANIC on raw byte strings that may be invalid UTF-8 at the source
    /// but are lossily converted — guards the parser against odd inputs a
    /// caller might funnel in from the wire.
    #[test]
    fn from_str_from_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let s = String::from_utf8_lossy(&bytes);
        let _ = SampleFormat::from_str(&s);
    }

    /// ROUND-TRIP: `Display` then `from_str` reproduces an equal value.
    ///
    /// `bits`/`channels` are kept within `u16` and `rate` within `u32`
    /// (their storage widths), matching parser-accepted in-range values.
    #[test]
    fn display_round_trip(
        rate in any::<u32>(),
        bits in any::<u16>(),
        channels in any::<u16>(),
    ) {
        let sf = SampleFormat::new(rate, bits, channels);
        let text = sf.to_string();
        let parsed = SampleFormat::from_str(&text)
            .expect("Display output must parse back");
        prop_assert_eq!(sf, parsed);
        // Accessors survive the round-trip.
        prop_assert_eq!(parsed.rate(), rate);
        prop_assert_eq!(parsed.bits(), bits);
        prop_assert_eq!(parsed.channels(), channels);
    }

    /// ROUND-TRIP via a hand-built `"rate:bits:channels"` string: parsing
    /// yields the expected accessors. Values are in parser-accepted range.
    #[test]
    fn parse_decimal_triplet_accessors(
        rate in any::<u32>(),
        bits in any::<u16>(),
        channels in any::<u16>(),
    ) {
        let text = format!("{rate}:{bits}:{channels}");
        let sf = SampleFormat::from_str(&text).expect("valid triplet must parse");
        prop_assert_eq!(sf.rate(), rate);
        prop_assert_eq!(sf.bits(), bits);
        prop_assert_eq!(sf.channels(), channels);
        prop_assert_eq!(sf, SampleFormat::new(rate, bits, channels));
    }

    /// The `*` wildcard maps to 0 in any position, matching the C++ semantics.
    #[test]
    fn wildcard_fields_parse_to_zero(
        rate in any::<u32>(),
        bits in any::<u16>(),
        channels in any::<u16>(),
        rate_wild in any::<bool>(),
        bits_wild in any::<bool>(),
        chan_wild in any::<bool>(),
    ) {
        let rate_tok = if rate_wild { "*".to_string() } else { rate.to_string() };
        let bits_tok = if bits_wild { "*".to_string() } else { bits.to_string() };
        let chan_tok = if chan_wild { "*".to_string() } else { channels.to_string() };
        let text = format!("{rate_tok}:{bits_tok}:{chan_tok}");

        let sf = SampleFormat::from_str(&text).expect("wildcarded triplet must parse");
        prop_assert_eq!(sf.rate(), if rate_wild { 0 } else { rate });
        prop_assert_eq!(sf.bits(), if bits_wild { 0 } else { bits });
        prop_assert_eq!(sf.channels(), if chan_wild { 0 } else { channels });
    }

    /// Explicit `0` literals are accepted and preserved as zeros (same
    /// "unspecified" sentinel semantics as `*` at higher layers).
    #[test]
    fn zero_literals_are_accepted_and_preserved(
        rate in any::<u32>(),
        channels in any::<u16>(),
    ) {
        let text = format!("{rate}:0:{channels}");
        let sf = SampleFormat::from_str(&text).expect("zero-literal triplet must parse");
        prop_assert_eq!(sf.rate(), rate);
        prop_assert_eq!(sf.bits(), 0);
        prop_assert_eq!(sf.channels(), channels);

        let all_zero = SampleFormat::from_str("0:0:0").expect("all-zero triplet must parse");
        prop_assert_eq!(all_zero, SampleFormat::default());
    }

    /// Out-of-range bits/channels are rejected with `InvalidFormat` (checked
    /// u16 conversion), not accepted and not `Ok`.
    #[test]
    fn out_of_range_u16_fields_are_invalid_format(
        rate in any::<u32>(),
        too_large in 65536u32..=u32::MAX,
    ) {
        let bits_text = format!("{rate}:{too_large}:2");
        let err = SampleFormat::from_str(&bits_text).expect_err("bits > u16::MAX must fail");
        prop_assert!(
            matches!(err, SampleFormatError::InvalidFormat(_)),
            "expected InvalidFormat for out-of-range bits, got {err:?}"
        );

        let channels_text = format!("{rate}:16:{too_large}");
        let err = SampleFormat::from_str(&channels_text).expect_err("channels > u16::MAX must fail");
        prop_assert!(
            matches!(err, SampleFormatError::InvalidFormat(_)),
            "expected InvalidFormat for out-of-range channels, got {err:?}"
        );
    }

    /// Numeric-token parse failures (including signed negatives) surface as
    /// `InvalidNumber`.
    #[test]
    fn invalid_numeric_tokens_report_invalid_number(
        rate in any::<u32>(),
        bad_tok in prop_oneof![
            Just("-1".to_string()),
            Just("+".to_string()),
            Just("abc".to_string()),
            Just("12x".to_string()),
        ],
    ) {
        let text = format!("{rate}:{bad_tok}:2");
        let err = SampleFormat::from_str(&text).expect_err("invalid numeric token must fail");
        prop_assert!(
            matches!(err, SampleFormatError::InvalidNumber(_)),
            "expected InvalidNumber, got {err:?}"
        );
    }

    /// Strings whose colon-part count is not exactly 3 are always rejected with
    /// `InvalidFormat` — never a panic and never a spurious `Ok`.
    #[test]
    fn wrong_part_count_is_invalid_format(
        parts in prop::collection::vec("[0-9]{1,4}", 0..=6)
            .prop_filter("need != 3 parts", |v| v.len() != 3),
    ) {
        let text = parts.join(":");
        let err = SampleFormat::from_str(&text)
            .expect_err("non-3-part string must be an error");
        prop_assert!(
            matches!(err, snapcast_proto::sample_format::SampleFormatError::InvalidFormat(_)),
            "expected InvalidFormat, got {err:?}"
        );
    }
}
