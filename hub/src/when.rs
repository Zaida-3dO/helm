//! Reading dates out of a search query: "a few months ago", "last week", "since 2026-03-01".
//!
//! [`parse`] takes the query as it was typed and the caller's idea of now, and returns the
//! query with the date phrase taken out plus the [`Window`] that phrase describes, as
//! millisecond bounds (inclusive start, exclusive end) on a chunk's `occurred_at`. The
//! caller passes `utc_offset_minutes` for the moment `now_ms` falls in, already resolved from
//! its own timezone rule; this module does no timezone work of its own — it treats the offset
//! as constant across the whole window, which shifts a long window's far edge by at most the
//! one summer-time hour.
//!
//! One window per query: the left-most date phrase wins and the rest of the text stays as the
//! search words. `vague` marks a phrase that was a rough range ("a few months ago", "recently",
//! a month named without a year) rather than a date the writer chose exactly.

/// A time range with inclusive start and exclusive end, in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from_ms: i64,
    pub to_ms: i64,
    pub vague: bool,
}

/// Find the first date phrase in `query`, remove it, and return the window it names.
/// The query comes back stripped and tidied; when there is no phrase the text is
/// returned untouched with `None`.
pub fn parse(query: &str, now_ms: i64, utc_offset_minutes: i32) -> (String, Option<Window>) {
    let offset_ms = utc_offset_minutes as i64 * 60_000;
    let now = civil_of(now_ms + offset_ms);
    let tokens = tokenize(query);
    let Some((first, last, phrase)) = find_phrase(&tokens) else {
        return (query.to_string(), None);
    };
    let window = match phrase {
        Phrase::Today => day_window(now, 0, offset_ms),
        Phrase::Yesterday => day_window(now, 1, offset_ms),
        Phrase::Week(back) => week_window(now, back, offset_ms),
        Phrase::MonthThis(back) => month_window(now, back, offset_ms, false),
        Phrase::YearThis => Window {
            from_ms: at_midnight(civil(now.y, 1, 1), offset_ms),
            to_ms: at_midnight(civil(now.y + 1, 1, 1), offset_ms),
            vague: false,
        },
        Phrase::YearLast => Window {
            from_ms: at_midnight(civil(now.y - 1, 1, 1), offset_ms),
            to_ms: at_midnight(civil(now.y, 1, 1), offset_ms),
            vague: false,
        },
        Phrase::Recent => Window {
            from_ms: now_ms - 30 * DAY_MS,
            to_ms: now_ms,
            vague: true,
        },
        Phrase::Ago(unit, n) => match unit {
            Unit::Days => day_window(now, n as i64, offset_ms),
            Unit::Weeks => week_window(now, n as i64, offset_ms),
            Unit::Months => month_window(now, n, offset_ms, false),
        },
        Phrase::VagueAgo(unit, few) => vague_ago_window(now_ms, unit, few, offset_ms),
        Phrase::MonthOfYear(y, m) => month_span(y, m, offset_ms, false),
        Phrase::GuessedMonth(m) => {
            let y = if m <= now.m { now.y } else { now.y - 1 };
            month_span(y, m, offset_ms, true)
        }
        Phrase::Date(c, month_only) => {
            if month_only {
                month_span(c.y, c.m, offset_ms, false)
            } else {
                let from = at_midnight(c, offset_ms);
                Window {
                    from_ms: from,
                    to_ms: from + DAY_MS,
                    vague: false,
                }
            }
        }
        Phrase::Since(c) => Window {
            from_ms: at_midnight(c, offset_ms),
            to_ms: now_ms,
            vague: false,
        },
        Phrase::Before(c) => Window {
            from_ms: i64::MIN,
            to_ms: at_midnight(c, offset_ms),
            vague: false,
        },
    };
    let from = tokens[first].start;
    let mut to = tokens[last].end;
    // Take a possessive "'s" (or a bare "'") that the phrase leaves behind.
    if query[to..].starts_with("'s") {
        to += 2;
    } else if query[to..].starts_with('’') {
        to += if query[to + 3..].starts_with('s') {
            4
        } else {
            3
        };
    } else if query[to..].starts_with('\'') {
        to += 1;
    }
    let mut stripped = String::new();
    stripped.push_str(&query[..from]);
    stripped.push_str(&query[to..]);
    (tidy(&stripped), Some(window))
}

const DAY_MS: i64 = 86_400_000;
const WEEK_MS: i64 = 7 * DAY_MS;

/// A civil date with no timezone attached: it means "year, month and day on the caller's wall".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    y: i32,
    m: u32,
    d: u32,
}

fn civil(y: i32, m: u32, d: u32) -> Civil {
    Civil { y, m, d }
}

/// Howard Hinnant's days-from-civil and civil-from-days: calendar math without a crate.
fn days_from_civil(c: Civil) -> i64 {
    let y = (c.y - if c.m <= 2 { 1 } else { 0 }) as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (c.m + 9) % 12;
    let doy = (153 * mp as u64 + 2) / 5 + c.d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn civil_from_days(z: i64) -> Civil {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    civil((y + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

/// Year, month and day of `ms` when shown on a wall clock `utc_offset_minutes` east of UTC.
fn civil_of(ms: i64) -> Civil {
    let local = if ms >= 0 { ms } else { ms - (DAY_MS - 1) };
    civil_from_days(local / DAY_MS)
}

fn at_midnight(c: Civil, offset_ms: i64) -> i64 {
    days_from_civil(c) * DAY_MS - offset_ms
}

fn weekday_mon0(c: Civil) -> i64 {
    (days_from_civil(c) + 3).rem_euclid(7)
}

/// `n` calendar months away, on the same day of the month where that month has one
/// (15 November minus 6 months is 15 May).
fn add_months(now: Civil, n: i32) -> Civil {
    let zero = now.y as i64 * 12 + now.m as i64 - 1 + n as i64;
    let y = zero.div_euclid(12) as i32;
    let m = (zero.rem_euclid(12) + 1) as u32;
    civil(y, m, now.d.min(month_len(y, m)))
}

fn month_len(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(civil(ny, nm, 1)) - days_from_civil(civil(y, m, 1))) as u32
}

fn day_window(now: Civil, back: i64, offset_ms: i64) -> Window {
    let from_day = days_from_civil(civil(now.y, now.m, now.d)) - back;
    Window {
        from_ms: from_day * DAY_MS - offset_ms,
        to_ms: (from_day + 1) * DAY_MS - offset_ms,
        vague: false,
    }
}

fn week_window(now: Civil, back: i64, offset_ms: i64) -> Window {
    let monday = days_from_civil(now) - weekday_mon0(now) - back * 7;
    Window {
        from_ms: monday * DAY_MS - offset_ms,
        to_ms: (monday + 7) * DAY_MS - offset_ms,
        vague: false,
    }
}

fn month_window(now: Civil, back: i32, offset_ms: i64, vague: bool) -> Window {
    let start = add_months(now, -back);
    month_span(start.y, start.m, offset_ms, vague)
}

fn month_span(y: i32, m: u32, offset_ms: i64, vague: bool) -> Window {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Window {
        from_ms: at_midnight(civil(y, m, 1), offset_ms),
        to_ms: at_midnight(civil(ny, nm, 1), offset_ms),
        vague,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Days,
    Weeks,
    Months,
}

/// The vague forms get set ranges measured from this instant: a few days is 1–3 days back,
/// a few weeks 1–3 weeks, a few months 6 weeks to 6 months; a couple of days is 1–2 days
/// back, a couple of weeks 1–4, a couple of months 3 weeks to 3 months.
fn vague_ago_window(now_ms: i64, unit: Unit, few: bool, offset_ms: i64) -> Window {
    let (from_ms, to_ms) = match (unit, few) {
        (Unit::Days, true) => (now_ms - 3 * DAY_MS, now_ms - DAY_MS),
        (Unit::Days, false) => (now_ms - 2 * DAY_MS, now_ms - DAY_MS),
        (Unit::Weeks, true) => (now_ms - 3 * WEEK_MS, now_ms - WEEK_MS),
        (Unit::Weeks, false) => (now_ms - 4 * WEEK_MS, now_ms - WEEK_MS),
        (Unit::Months, true) => {
            let c = civil_of(now_ms + offset_ms);
            (
                at_midnight(add_months(c, -6), offset_ms),
                now_ms - 6 * WEEK_MS,
            )
        }
        (Unit::Months, false) => {
            let c = civil_of(now_ms + offset_ms);
            (
                at_midnight(add_months(c, -3), offset_ms),
                now_ms - 3 * WEEK_MS,
            )
        }
    };
    Window {
        from_ms,
        to_ms,
        vague: true,
    }
}

/// The phrase shapes a token run can match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phrase {
    Today,
    Yesterday,
    Week(i64),
    MonthThis(i32),
    YearThis,
    YearLast,
    Recent,
    Ago(Unit, i32),
    VagueAgo(Unit, bool),
    MonthOfYear(i32, u32),
    GuessedMonth(u32),
    Date(Civil, bool),
    Since(Civil),
    Before(Civil),
}

struct Token {
    word: String,
    start: usize,
    end: usize,
}

/// Words, numbers, and whole `YYYY-MM[-DD]` runs; every token starts and ends at a
/// non-alphanumeric edge, which gives word boundaries for free ("maybe" is one token,
/// never "may" plus "be").
fn tokenize(q: &str) -> Vec<Token> {
    let lower = q.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let glued = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.');
        if !glued {
            if let Some(len) = date_run_len(&lower[i..]) {
                tokens.push(Token {
                    word: lower[i..i + len].to_string(),
                    start: i,
                    end: i + len,
                });
                i += len;
                continue;
            }
        }
        let b = bytes[i];
        if b.is_ascii_alphanumeric() {
            let start = i;
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
                end += 1;
            }
            tokens.push(Token {
                word: lower[start..end].to_string(),
                start,
                end,
            });
            i = end;
        } else {
            i += 1;
        }
    }
    tokens
}

/// Length of a leading `YYYY-MM-DD` or `YYYY-MM` (exactly this shape), else 0. A trailing
/// separator keeps the token from eating into "2026-03-07x".
fn date_run_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let digits = |k: usize| b.get(k).is_some_and(|c| c.is_ascii_digit());
    if b.len() >= 7 && (0..4).all(digits) && b[4] == b'-' && digits(5) && digits(6) {
        if b.len() >= 10 && b[7] == b'-' && digits(8) && digits(9) {
            if b.len() == 10 || !b[10].is_ascii_alphanumeric() {
                return Some(10);
            }
        } else if b.len() == 7 || !(b[7].is_ascii_alphanumeric() || b[7] == b'-') {
            return Some(7);
        }
    }
    None
}

fn date_from_token(w: &str) -> Option<(Civil, bool)> {
    let parts: Vec<&str> = w.split('-').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let month_only = parts.len() == 2;
    let d: u32 = if month_only {
        1
    } else {
        parts[2].parse().ok()?
    };
    if parts[0].len() != 4
        || parts[1].len() != 2
        || (!month_only && parts[2].len() != 2)
        || !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
    {
        return None;
    }
    Some((civil(y, m, d), month_only))
}

fn month_name(w: &str) -> Option<u32> {
    Some(match w {
        "january" => 1,
        "february" => 2,
        "march" => 3,
        "april" => 4,
        "may" => 5,
        "june" => 6,
        "july" => 7,
        "august" => 8,
        "september" => 9,
        "october" => 10,
        "november" => 11,
        "december" => 12,
        _ => return None,
    })
}

fn year_num(w: &str) -> Option<i32> {
    if w.len() == 4 && w.bytes().all(|c| c.is_ascii_digit()) {
        w.parse().ok()
    } else {
        None
    }
}

fn num_word(w: &str) -> Option<i32> {
    if w.is_empty() {
        return None;
    }
    if w.bytes().all(|c| c.is_ascii_digit()) {
        return w.parse().ok();
    }
    Some(match w {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => return None,
    })
}

fn unit_word(w: &str) -> Option<Unit> {
    Some(match w {
        "day" | "days" => Unit::Days,
        "week" | "weeks" => Unit::Weeks,
        "month" | "months" => Unit::Months,
        _ => return None,
    })
}

/// The left-most phrase wins; at one spot the longer reading wins.
fn find_phrase(tokens: &[Token]) -> Option<(usize, usize, Phrase)> {
    (0..tokens.len()).find_map(|i| match_at(tokens, i))
}

fn match_at(tokens: &[Token], i: usize) -> Option<(usize, usize, Phrase)> {
    let w = tokens[i].word.as_str();
    let t = |j: usize| tokens.get(j).map(|x| x.word.as_str()).unwrap_or("");
    match w {
        "today" => return Some((i, i, Phrase::Today)),
        "yesterday" => return Some((i, i, Phrase::Yesterday)),
        "recently" | "lately" => return Some((i, i, Phrase::Recent)),
        _ => {}
    }
    if (w == "this" || w == "last") && matches!(t(i + 1), "week" | "weeks") {
        return Some((i, i + 1, Phrase::Week(if w == "last" { 1 } else { 0 })));
    }
    if (w == "this" || w == "last") && matches!(t(i + 1), "month" | "months") {
        return Some((i, i + 1, Phrase::MonthThis(if w == "last" { 1 } else { 0 })));
    }
    if w == "last" && t(i + 1) == "year" {
        return Some((i, i + 1, Phrase::YearLast));
    }
    if w == "this" && t(i + 1) == "year" {
        return Some((i, i + 1, Phrase::YearThis));
    }
    if w == "in" {
        if let Some(m) = month_name(t(i + 1)) {
            if let Some(y) = year_num(t(i + 2)) {
                return Some((i, i + 2, Phrase::MonthOfYear(y, m)));
            }
            return Some((i, i + 1, Phrase::GuessedMonth(m)));
        }
    }
    if let Some(m) = month_name(w) {
        if let Some(y) = year_num(t(i + 1)) {
            return Some((i, i + 1, Phrase::MonthOfYear(y, m)));
        }
    }
    if (w == "since" || w == "before") && !t(i + 1).is_empty() {
        if let Some((c, _)) = date_from_token(t(i + 1)) {
            return Some((
                i,
                i + 1,
                if w == "since" {
                    Phrase::Since(c)
                } else {
                    Phrase::Before(c)
                },
            ));
        }
    }
    if let Some(unit) = unit_word(t(i + 1)) {
        if let Some(n) = num_word(w) {
            if t(i + 2) == "ago" {
                return Some((i, i + 2, Phrase::Ago(unit, n)));
            }
        }
    }
    if w == "a" && matches!(t(i + 1), "few" | "couple") {
        let few = t(i + 1) == "few";
        let mut j = i + 2;
        if !few && t(j) == "of" {
            j += 1;
        }
        if let Some(unit) = unit_word(t(j)) {
            if t(j + 1) == "ago" {
                return Some((i, j + 1, Phrase::VagueAgo(unit, few)));
            }
        }
    }
    if let Some((c, month_only)) = date_from_token(w) {
        return Some((i, i, Phrase::Date(c, month_only)));
    }
    None
}

/// Rejoin the query: collapse the spaces the removed phrase leaves behind, and drop
/// orphan punctuation (a lone "'s" cannot happen, but a lone "'" can).
fn tidy(s: &str) -> String {
    let mut out = String::new();
    for part in s.split_whitespace() {
        if !part.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(y: i32, mo: u32, d: u32, hh: u32, mi: u32) -> i64 {
        at_midnight(civil(y, mo, d), 0) + hh as i64 * 3_600_000 + mi as i64 * 60_000
    }

    fn win(from: (i32, u32, u32), to: (i32, u32, u32)) -> (i64, i64) {
        (ms(from.0, from.1, from.2, 0, 0), ms(to.0, to.1, to.2, 0, 0))
    }

    // Sunday 15 November 2026, 10:30 UTC.
    fn now() -> i64 {
        ms(2026, 11, 15, 10, 30)
    }

    #[test]
    fn phrase_table() {
        // (query, stripped, window bounds or (0,0) for none, vague)
        let rows = [
            // named days
            (
                "what did we say today",
                "what did we say",
                win((2026, 11, 15), (2026, 11, 16)),
                false,
            ),
            (
                "yesterday's deploy",
                "deploy",
                win((2026, 11, 14), (2026, 11, 15)),
                false,
            ),
            // weeks are Monday-first; 15 Nov 2026 is a Sunday
            (
                "this week's notes",
                "notes",
                win((2026, 11, 9), (2026, 11, 16)),
                false,
            ),
            (
                "last week's work",
                "work",
                win((2026, 11, 2), (2026, 11, 9)),
                false,
            ),
            (
                "last week end of week",
                "end of week",
                win((2026, 11, 2), (2026, 11, 9)),
                false,
            ),
            // months and years
            (
                "this month's decisions",
                "decisions",
                win((2026, 11, 1), (2026, 12, 1)),
                false,
            ),
            (
                "last month's release",
                "release",
                win((2026, 10, 1), (2026, 11, 1)),
                false,
            ),
            (
                "last year's lesson",
                "lesson",
                win((2025, 1, 1), (2026, 1, 1)),
                false,
            ),
            (
                "this year so far",
                "so far",
                win((2026, 1, 1), (2027, 1, 1)),
                false,
            ),
            // N units ago: one day, that week, that whole month
            ("3 days ago", "", win((2026, 11, 12), (2026, 11, 13)), false),
            (
                "two weeks ago",
                "",
                win((2026, 10, 26), (2026, 11, 2)),
                false,
            ),
            ("1 month ago", "", win((2026, 10, 1), (2026, 11, 1)), false),
            ("11 months ago", "", win((2025, 12, 1), (2026, 1, 1)), false),
            // vague ranges measured from the instant
            (
                "a few days ago we saw",
                "we saw",
                (now() - 3 * DAY_MS, now() - DAY_MS),
                true,
            ),
            (
                "a few weeks ago",
                "",
                (now() - 3 * WEEK_MS, now() - WEEK_MS),
                true,
            ),
            (
                "a few months ago",
                "",
                (ms(2026, 5, 15, 0, 0), now() - 6 * WEEK_MS),
                true,
            ),
            (
                "a couple days ago",
                "",
                (now() - 2 * DAY_MS, now() - DAY_MS),
                true,
            ),
            (
                "a couple of weeks ago",
                "",
                (now() - 4 * WEEK_MS, now() - WEEK_MS),
                true,
            ),
            (
                "a couple of months ago",
                "",
                (ms(2026, 8, 15, 0, 0), now() - 3 * WEEK_MS),
                true,
            ),
            (
                "recently filed",
                "filed",
                (now() - 30 * DAY_MS, now()),
                true,
            ),
            // months named, with and without a year
            (
                "in June we shipped",
                "we shipped",
                win((2026, 6, 1), (2026, 7, 1)),
                true,
            ),
            ("in june 2025", "", win((2025, 6, 1), (2025, 7, 1)), false),
            (
                "June 2025 notes",
                "notes",
                win((2025, 6, 1), (2025, 7, 1)),
                false,
            ),
            ("in December", "", win((2025, 12, 1), (2026, 1, 1)), true),
            ("in March", "", win((2026, 3, 1), (2026, 4, 1)), true),
            // written dates
            (
                "back on 2026-03-07",
                "back on",
                win((2026, 3, 7), (2026, 3, 8)),
                false,
            ),
            (
                "during 2025-04",
                "during",
                win((2025, 4, 1), (2025, 5, 1)),
                false,
            ),
            ("since 2026-03-01", "", (ms(2026, 3, 1, 0, 0), now()), false),
            (
                "before 2026-06",
                "",
                (i64::MIN, ms(2026, 6, 1, 0, 0)),
                false,
            ),
            // must NOT read as dates
            ("maybe later", "maybe later", (0, 0), false),
            ("the may release", "the may release", (0, 0), false),
            ("agony about agony", "agony about agony", (0, 0), false),
            ("a way forward", "a way forward", (0, 0), false),
            ("a few of the notes", "a few of the notes", (0, 0), false),
            ("5 minutes ago", "5 minutes ago", (0, 0), false),
            ("2026", "2026", (0, 0), false),
            ("2026-1", "2026-1", (0, 0), false),
            ("13-2026", "13-2026", (0, 0), false),
            ("2026-13-01", "2026-13-01", (0, 0), false),
            ("v1.2026-11", "v1.2026-11", (0, 0), false),
            (
                "how does the relay retry work",
                "how does the relay retry work",
                (0, 0),
                false,
            ),
        ];
        for (q, stripped, (from, to), vague) in rows {
            let (got_q, got_w) = parse(q, now(), 0);
            assert_eq!(got_q, stripped, "strip for {q:?}");
            match (got_w, (from, to)) {
                (None, (0, 0)) => {}
                (Some(w), bounds) => {
                    assert_eq!((w.from_ms, w.to_ms), bounds, "window for {q:?}");
                    assert_eq!(w.vague, vague, "vague for {q:?}");
                }
                (None, _) => panic!("expected a window for {q:?}"),
            }
        }
    }

    #[test]
    fn first_phrase_wins_and_the_rest_stays() {
        let (q, w) = parse("last week in June we debated", now(), 0);
        assert_eq!(q, "in June we debated");
        assert_eq!(w.unwrap().from_ms, ms(2026, 11, 2, 0, 0));
    }

    #[test]
    fn the_offset_moves_the_day_edges() {
        // 21:30 UTC with a +1 h wall is 22:30 local on the 15th.
        let (q, w) = parse("today", ms(2026, 11, 15, 21, 30), 60);
        assert_eq!(q, "");
        let w = w.unwrap();
        assert_eq!(w.from_ms, ms(2026, 11, 15, 0, 0) - 60 * 60_000);
        assert_eq!(w.to_ms, ms(2026, 11, 16, 0, 0) - 60 * 60_000);
    }

    #[test]
    fn an_hour_past_local_midnight_is_still_the_old_day_for_the_wall_clock() {
        // 00:30 UTC is 01:30 local at +1 h: "yesterday" is the 15th.
        let w = parse("yesterday", ms(2026, 11, 16, 0, 30), 60).1.unwrap();
        assert_eq!(
            (w.from_ms, w.to_ms),
            (
                ms(2026, 11, 15, 0, 0) - 60 * 60_000,
                ms(2026, 11, 16, 0, 0) - 60 * 60_000
            )
        );
    }

    #[test]
    fn month_name_guessing_stays_past_now() {
        // Now is November: November itself has begun, so "in November" is this year.
        let w = parse("in November", now(), 0).1.unwrap();
        assert_eq!((w.from_ms, w.to_ms), win((2026, 11, 1), (2026, 12, 1)));
        assert!(w.vague);
    }
}
