//! How search results are ordered after the keyword and vector lists are fused.
//!
//! Every constant that shapes a ranking is in this file, so tuning is a one-file change and the
//! ranking table at the bottom pins it. They are constants, not settings: a setting nobody tunes is
//! noise. The steps, in order:
//!
//! 1. [`apply`]: each candidate's fused score is multiplied by its source weight, by a down-weight
//!    when the item is another person's (shared with the viewer: the household shares one memory, so
//!    the person only decides the order, never what is found), and, for conversations asked about
//!    with no date window, by a recency factor. The weights are softened (see [`WEIGHT_SPREAD`]),
//!    so they order near-equal matches and never bury a direct hit.
//! 2. [`group`]: a document scores its best chunk plus a tenth of its second, and shows at most two
//!    chunks (a curated memory shows one).
//! 3. [`collapse`]: a chunk that is nearly the same as a higher-ranked one is dropped.
//!
//! The small date helpers at the end read and write UTC milliseconds without a calendar crate.

/// Reciprocal-rank-fusion constant (the usual value from the RRF paper).
pub const RRF_K: f64 = 60.0;
/// Source weights.
pub const MEMORY_WEIGHT: f64 = 1.0;
pub const NOTE_WEIGHT: f64 = 0.8;
pub const CONVERSATION_WEIGHT: f64 = 0.5;
pub const TRACE_WEIGHT: f64 = 0.35;
/// Another person's shared item, unless the search names that person.
pub const OTHER_PERSON_WEIGHT: f64 = 0.3;
/// How much of a source or person weight is kept: a weight moves a score only part of the way
/// from 1 down to the weight, so a passage that matched the words and the meaning is never
/// outranked by a memory that matched one of them weakly. Who may see what is decided by the
/// privacy filter before ranking, never by these weights.
pub const WEIGHT_SPREAD: f64 = 0.5;
/// Conversations lose half their weight every this many days, smoothly, so a recent chat is found
/// before an old one that matches about as well. The floor keeps an old chat findable forever: it
/// is never dropped, only ranked after the recent ones.
pub const RECENCY_HALF_LIFE_DAYS: f64 = 90.0;
pub const RECENCY_FLOOR: f64 = 0.2;
/// A document's second chunk counts for this much of its score.
pub const SECOND_CHUNK_WEIGHT: f64 = 0.1;
/// Chunks whose vectors are closer than this are the same passage.
pub const NEAR_DUPLICATE: f64 = 0.95;
/// Chunks one document shows.
pub const CHUNKS_PER_DOC: usize = 2;

/// A weight as ranking uses it: pulled toward 1 by [`WEIGHT_SPREAD`].
fn soften(weight: f64) -> f64 {
    1.0 - WEIGHT_SPREAD * (1.0 - weight)
}

pub fn source_weight(tier: &str) -> f64 {
    match tier {
        "memory" => MEMORY_WEIGHT,
        "note" => NOTE_WEIGHT,
        "trace" => TRACE_WEIGHT,
        _ => CONVERSATION_WEIGHT,
    }
}

/// One chunk that survived the keyword and vector lists.
#[derive(Debug, Clone)]
pub struct Cand {
    pub chunk: i64,
    pub doc: i64,
    pub tier: String,
    pub owner: Option<i64>,
    /// When the passage happened (milliseconds), if known.
    pub at: Option<i64>,
    /// The passage names the person the query is about (see `who.rs`).
    pub about: bool,
    /// The fused keyword and vector score, before the weights.
    pub rrf: f64,
    pub score: f64,
    pub keyword: bool,
    pub vector: bool,
    /// The chunk's vector, scaled to length one, for the near-duplicate check.
    pub unit: Option<Vec<f32>>,
}

/// What a search needs to weigh a candidate.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub viewer: i64,
    /// The person the search named, if it did: their items are not down-weighted.
    pub person: Option<i64>,
    /// The person the words of the query are about ("my flight", "Ben's flight"), if they say.
    /// Their records, and the ones that mention them, lead; every other person's, the asker's own
    /// included, are down-weighted like another person's. Who asks decides nothing else.
    pub focus: Option<i64>,
    pub now_ms: i64,
    /// A date window filtered the search, so age is already part of the question.
    pub windowed: bool,
}

/// The factor that turns a fused score into the ranked one.
pub fn multiplier(c: &Cand, ctx: &Context) -> f64 {
    let mut m = soften(source_weight(&c.tier));
    let theirs = match ctx.focus {
        Some(f) => c.owner != Some(f) && !c.about,
        None => c.owner != Some(ctx.viewer) && c.owner != ctx.person,
    };
    if theirs {
        m *= soften(OTHER_PERSON_WEIGHT);
    }
    if c.tier == "conversation" && !ctx.windowed {
        if let Some(at) = c.at {
            // A time in the future (past a day for clocks that disagree) is not a date a passage
            // can have: it earns no freshness, however far ahead a sender dates it.
            let age_days = if at > ctx.now_ms + 86_400_000 {
                f64::INFINITY
            } else {
                ((ctx.now_ms - at).max(0) as f64) / 86_400_000.0
            };
            m *= 0.5f64
                .powf(age_days / RECENCY_HALF_LIFE_DAYS)
                .max(RECENCY_FLOOR);
        }
    }
    m
}

pub fn apply(cands: &mut [Cand], ctx: &Context) {
    for c in cands.iter_mut() {
        c.score = c.rrf * multiplier(c, ctx);
    }
}

/// A document and the chunks of it that are shown.
#[derive(Debug, Clone)]
pub struct Group {
    pub doc: i64,
    pub score: f64,
    pub chunks: Vec<Cand>,
}

/// Best document first; ties break on the older document id so an order never wobbles.
pub fn group(cands: Vec<Cand>) -> Vec<Group> {
    let mut by_doc: Vec<Group> = Vec::new();
    for c in cands {
        match by_doc.iter_mut().find(|g| g.doc == c.doc) {
            Some(g) => g.chunks.push(c),
            None => by_doc.push(Group {
                doc: c.doc,
                score: 0.0,
                chunks: vec![c],
            }),
        }
    }
    for g in &mut by_doc {
        g.chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.chunk.cmp(&b.chunk))
        });
        let shown = if g.chunks[0].tier == "memory" {
            1
        } else {
            CHUNKS_PER_DOC
        };
        g.score = g.chunks[0].score
            + g.chunks
                .get(1)
                .map_or(0.0, |c| c.score * SECOND_CHUNK_WEIGHT);
        g.chunks.truncate(shown);
    }
    by_doc.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.doc.cmp(&b.doc))
    });
    by_doc
}

/// Drop each chunk whose vector is within [`NEAR_DUPLICATE`] of one ranked above it, and the
/// documents left with nothing.
pub fn collapse(groups: &mut Vec<Group>) {
    let mut kept: Vec<Vec<f32>> = Vec::new();
    for g in groups.iter_mut() {
        g.chunks.retain(|c| {
            let Some(u) = &c.unit else {
                return true;
            };
            if kept.iter().any(|k| dot(k, u) > NEAR_DUPLICATE) {
                return false;
            }
            kept.push(u.clone());
            true
        });
    }
    groups.retain(|g| !g.chunks.is_empty());
}

pub fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

/// `v` scaled to length one, or `None` for the zero vector.
pub fn unit(v: &[f32]) -> Option<Vec<f32>> {
    let n = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    (n > 0.0).then(|| v.iter().map(|x| (f64::from(*x) / n) as f32).collect())
}

// ---- dates ----

const DAY_MS: i64 = 86_400_000;

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count from [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-11-01`, or `2026-11-01T09:30[:00[.fff]][Z]` (UTC), as milliseconds. The shapes are the
/// ones `memory_list`'s `since` takes.
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if !crate::store::is_iso_since(s) {
        return None;
    }
    let num = |a: usize, n: usize| s.get(a..a + n)?.parse::<i64>().ok();
    let days = days_from_civil(num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let mut ms = days * DAY_MS;
    if s.len() > 10 {
        ms += num(11, 2)? * 3_600_000 + num(14, 2)? * 60_000;
        if s.len() >= 19 {
            ms += num(17, 2)? * 1000;
        }
        if let Some(frac) = s.get(19..).and_then(|r| r.strip_prefix('.')) {
            let digits: String = frac
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();
            ms += format!("{digits:0<3}").parse::<i64>().ok()?;
        }
    }
    Some(ms)
}

/// `2026-11-01T09:30:00Z`.
pub fn iso_from_ms(ms: i64) -> String {
    let days = ms.div_euclid(DAY_MS);
    let rest = ms.rem_euclid(DAY_MS) / 1000;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The minutes a time zone is ahead of UTC at `now_ms`. Only UTC and `Europe/London` are known
/// (the container's `TZ`); anything else reads as UTC rather than guessing.
pub fn utc_offset_minutes(tz: &str, now_ms: i64) -> i32 {
    if tz.trim() != "Europe/London" {
        return 0;
    }
    let (year, _, _) = civil_from_days(now_ms.div_euclid(DAY_MS));
    // Summer time runs from 01:00 UTC on the last Sunday of March to the same hour on the last
    // Sunday of October.
    let last_sunday = |month: i64| {
        let last = days_from_civil(year, month, 31);
        // 1970-01-01 was a Thursday; 0 is Sunday.
        last - (last + 4).rem_euclid(7)
    };
    let start = last_sunday(3) * DAY_MS + 3_600_000;
    let end = last_sunday(10) * DAY_MS + 3_600_000;
    if (start..end).contains(&now_ms) {
        60
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(chunk: i64, doc: i64, tier: &str, owner: i64, rrf: f64) -> Cand {
        Cand {
            chunk,
            doc,
            tier: tier.into(),
            owner: Some(owner),
            at: None,
            about: false,
            rrf,
            score: 0.0,
            keyword: true,
            vector: false,
            unit: None,
        }
    }

    #[test]
    fn weights_order_sources_people_and_age() {
        let now = 1_800_000_000_000;
        let ctx = |person, windowed| Context {
            viewer: 1,
            person,
            focus: None,
            now_ms: now,
            windowed,
        };
        let aged = |days: i64| Cand {
            at: Some(now - days * DAY_MS),
            ..cand(1, 1, "conversation", 1, 1.0)
        };
        for (label, c, ctx, want) in [
            (
                "memory",
                cand(1, 1, "memory", 1, 1.0),
                ctx(None, false),
                1.0,
            ),
            ("note", cand(1, 1, "note", 1, 1.0), ctx(None, false), 0.9),
            (
                "conversation, no date",
                cand(1, 1, "conversation", 1, 1.0),
                ctx(None, false),
                0.75,
            ),
            (
                "trace",
                cand(1, 1, "trace", 1, 1.0),
                ctx(None, false),
                0.675,
            ),
            (
                "someone else's memory",
                cand(1, 1, "memory", 2, 1.0),
                ctx(None, false),
                0.65,
            ),
            (
                "someone else's, named",
                cand(1, 1, "memory", 2, 1.0),
                ctx(Some(2), false),
                1.0,
            ),
            (
                "someone else's, another named",
                cand(1, 1, "memory", 2, 1.0),
                ctx(Some(3), false),
                0.65,
            ),
            ("fresh conversation", aged(0), ctx(None, false), 0.75),
            ("one half-life", aged(90), ctx(None, false), 0.375),
            ("two half-lives", aged(180), ctx(None, false), 0.1875),
            (
                "a month",
                aged(30),
                ctx(None, false),
                0.75 * 0.5f64.powf(1.0 / 3.0),
            ),
            ("a year, at the floor", aged(365), ctx(None, false), 0.15),
            ("very old, at the floor", aged(2000), ctx(None, false), 0.15),
            ("old but windowed", aged(2000), ctx(None, true), 0.75),
            // A date days ahead is not one a passage can have: it earns no freshness.
            ("a week ahead", aged(-7), ctx(None, false), 0.15),
            ("a century ahead", aged(-36_500), ctx(None, false), 0.15),
        ] {
            let got = multiplier(&c, &ctx);
            assert!((got - want).abs() < 1e-9, "{label}: {got} != {want}");
        }
        // The decay is smooth: each step older ranks strictly lower until the floor, never zero.
        let steps: Vec<f64> = [0, 7, 30, 90, 180]
            .iter()
            .map(|d| multiplier(&aged(*d), &ctx(None, false)))
            .collect();
        assert!(steps.windows(2).all(|w| w[0] > w[1]), "{steps:?}");
        assert!(multiplier(&aged(100_000), &ctx(None, false)) > 0.0);
    }

    #[test]
    fn a_query_about_a_person_prefers_their_records_whoever_asks() {
        let ctx = |viewer, focus| Context {
            viewer,
            person: None,
            focus,
            now_ms: 0,
            windowed: true,
        };
        let mentions = |owner| Cand {
            about: true,
            ..cand(1, 1, "note", owner, 1.0)
        };
        // Person 1 asks; the document is owned by 1 or 2.
        for (label, c, ctx, want) in [
            (
                "no focus: the asker's own",
                cand(1, 1, "note", 1, 1.0),
                ctx(1, None),
                0.9,
            ),
            (
                "no focus: someone else's",
                cand(1, 1, "note", 2, 1.0),
                ctx(1, None),
                0.9 * 0.65,
            ),
            (
                "about me: mine",
                cand(1, 1, "note", 1, 1.0),
                ctx(1, Some(1)),
                0.9,
            ),
            (
                "about me: theirs",
                cand(1, 1, "note", 2, 1.0),
                ctx(1, Some(1)),
                0.9 * 0.65,
            ),
            (
                "about them: theirs leads",
                cand(1, 1, "note", 2, 1.0),
                ctx(1, Some(2)),
                0.9,
            ),
            (
                "about them: mine falls back",
                cand(1, 1, "note", 1, 1.0),
                ctx(1, Some(2)),
                0.9 * 0.65,
            ),
            (
                "about them: mine, but it names them",
                mentions(1),
                ctx(1, Some(2)),
                0.9,
            ),
            (
                "about me: theirs, but it names me",
                mentions(2),
                ctx(1, Some(1)),
                0.9,
            ),
        ] {
            let got = multiplier(&c, &ctx);
            assert!((got - want).abs() < 1e-9, "{label}: {got} != {want}");
        }
    }

    #[test]
    fn documents_group_by_best_chunk_and_near_copies_collapse() {
        let ctx = Context {
            viewer: 1,
            person: None,
            focus: None,
            now_ms: 0,
            windowed: true,
        };
        let mut v = vec![
            cand(1, 10, "conversation", 1, 0.020),
            cand(2, 10, "conversation", 1, 0.018),
            cand(3, 10, "conversation", 1, 0.017),
            cand(4, 11, "note", 1, 0.030),
            cand(5, 12, "memory", 1, 0.010),
            cand(6, 12, "memory", 1, 0.009),
        ];
        apply(&mut v, &ctx);
        let groups = group(v);
        let shape: Vec<(i64, Vec<i64>)> = groups
            .iter()
            .map(|g| (g.doc, g.chunks.iter().map(|c| c.chunk).collect()))
            .collect();
        // The note leads (0.027); the chat's 0.015 plus a tenth of its second chunk (0.0135 x 0.1)
        // is ahead of the memory's 0.010. A chat shows two chunks and a memory one.
        assert_eq!(shape, vec![(11, vec![4]), (10, vec![1, 2]), (12, vec![5])]);
        assert!((groups[1].score - (0.015 + 0.1 * 0.0135)).abs() < 1e-9);

        let same = unit(&[1.0, 0.1]);
        let mut groups = group(vec![
            Cand {
                unit: same.clone(),
                ..cand(1, 1, "memory", 1, 0.03)
            },
            Cand {
                unit: unit(&[1.0, 0.12]),
                ..cand(2, 2, "note", 1, 0.02)
            },
            Cand {
                unit: unit(&[0.0, 1.0]),
                ..cand(3, 3, "note", 1, 0.01)
            },
            cand(4, 4, "note", 1, 0.005),
        ]);
        // Scores were never applied, so order by the (zero) scores is by doc id.
        for (i, g) in groups.iter_mut().enumerate() {
            g.chunks[0].score = 1.0 - i as f64 / 10.0;
        }
        collapse(&mut groups);
        let left: Vec<i64> = groups.iter().map(|g| g.doc).collect();
        assert_eq!(
            left,
            vec![1, 3, 4],
            "the near copy goes, a chunk without a vector stays"
        );
    }

    #[test]
    fn dates_round_trip_and_london_follows_summer_time() {
        for (iso, ms) in [
            ("1970-01-01", 0),
            ("2026-11-15", 1_794_700_800_000),
            ("2026-11-15T09:30", 1_794_700_800_000 + 34_200_000),
            ("2026-11-15T09:30:15.250Z", 1_794_700_800_000 + 34_215_250),
            ("2024-02-29", 1_709_164_800_000),
        ] {
            assert_eq!(parse_iso_ms(iso), Some(ms), "{iso}");
        }
        assert_eq!(parse_iso_ms("yesterday"), None);
        assert_eq!(
            iso_from_ms(1_794_700_800_000 + 34_215_000),
            "2026-11-15T09:30:15Z"
        );
        let at = |y: i64, m: i64, d: i64, h: i64| days_from_civil(y, m, d) * DAY_MS + h * 3_600_000;
        for (label, now, want) in [
            ("winter", at(2026, 1, 15, 12), 0),
            ("the hour before summer time", at(2026, 3, 29, 0), 0),
            ("summer time starts", at(2026, 3, 29, 1), 60),
            ("midsummer", at(2026, 7, 1, 12), 60),
            ("the hour before it ends", at(2026, 10, 25, 0), 60),
            ("it ends", at(2026, 10, 25, 1), 0),
        ] {
            assert_eq!(utc_offset_minutes("Europe/London", now), want, "{label}");
        }
        assert_eq!(utc_offset_minutes("Asia/Tokyo", at(2026, 7, 1, 12)), 0);
    }
}
