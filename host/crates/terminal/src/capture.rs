//! Picking the Claude token out of a sign-in session's output.
//!
//! `claude setup-token` prints the token it made on a line of its own. The session shows that
//! output as drawn text: colours and cursor moves around it, and a long token may be wrapped over
//! two rows when the terminal is narrow. [`find`] reads the text as a person would: it drops the
//! escape sequences, treats a cursor move to another row as a line break, and joins a token row to
//! the next one only when the row is as wide as the terminal (a wrapped row), never across a
//! blank or short row. It answers only once the token is known to be complete, so a token that is
//! still arriving is never saved in part.

use helm_core::chat_token;
use std::ops::Range;

/// A token found in some output: the token, and the bytes of the output it covers (escape
/// sequences and line breaks inside a wrapped token included), so the caller can blank them.
#[derive(PartialEq, Eq)]
pub struct Found {
    pub token: String,
    pub span: Range<usize>,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Found(.., {:?})", self.span)
    }
}

/// One visible character of the output, and the bytes it came from.
struct Cell {
    ch: char,
    at: usize,
    end: usize,
}

/// The visible text split into rows. A row holds its characters only.
fn rows(text: &str) -> (Vec<Vec<Cell>>, Vec<bool>) {
    let bytes = text.as_bytes();
    let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
    // `ended[i]`: row `i` was closed by a line break (the last row is still open until one comes).
    let mut ended: Vec<bool> = Vec::new();
    let mut i = 0;
    let br = |rows: &mut Vec<Vec<Cell>>, ended: &mut Vec<bool>| {
        ended.push(true);
        rows.push(Vec::new());
    };
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            let next = bytes.get(i + 1).copied();
            match next {
                // A control sequence: parameters, then one final byte.
                Some(b'[') => {
                    let mut j = i + 2;
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    // A cursor move to another row starts a new line.
                    if j < bytes.len()
                        && matches!(bytes[j], b'H' | b'f' | b'A' | b'B' | b'E' | b'F' | b'd')
                    {
                        br(&mut rows, &mut ended);
                    }
                    i = (j + 1).min(bytes.len());
                }
                // An operating-system command: up to a bell or the string terminator.
                Some(b']') => {
                    let mut j = i + 2;
                    while j < bytes.len() {
                        if bytes[j] == 0x07 {
                            j += 1;
                            break;
                        }
                        if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    i = j;
                }
                // Any other escape: it and the byte after it.
                Some(_) => i += 2,
                None => i += 1,
            }
            continue;
        }
        if b == b'\n' {
            br(&mut rows, &mut ended);
            i += 1;
            continue;
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
        let len = ch.len_utf8();
        rows.last_mut().unwrap().push(Cell {
            ch,
            at: i,
            end: i + len,
        });
        i += len;
    }
    (rows, ended)
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The row without trailing spaces (a terminal pads short rows).
fn trimmed(row: &[Cell]) -> &[Cell] {
    let end = row
        .iter()
        .rposition(|c| c.ch != ' ')
        .map_or(0, |last| last + 1);
    &row[..end]
}

/// The first complete Claude token in `text`, for a terminal `cols` wide. `None` when there is
/// none yet, or the last one found may still be growing.
pub fn find(text: &str, cols: usize) -> Option<Found> {
    let prefix: Vec<char> = "sk-ant-".chars().collect();
    let (rows, ended) = rows(text);
    for (r, row) in rows.iter().enumerate() {
        let row = trimmed(row);
        let chars: Vec<char> = row.iter().map(|c| c.ch).collect();
        let mut from = 0;
        while let Some(rel) = chars[from..]
            .windows(prefix.len())
            .position(|w| w == prefix.as_slice())
        {
            let start = from + rel;
            from = start + 1;
            let mut token = String::new();
            let mut first = row[start].at;
            let mut last = row[start].end;
            let mut run_end = start;
            while run_end < row.len() && token_char(row[run_end].ch) {
                token.push(row[run_end].ch);
                last = row[run_end].end;
                run_end += 1;
            }
            // Where the run stopped: before the row's end, the token is over.
            let mut closed = run_end < row.len();
            let mut at_row = r;
            let mut width = row.len();
            while !closed {
                // The run reached the end of the row. It goes on over the next row only when this
                // row is as wide as the terminal; a shorter row holds the whole token.
                let wrapped = cols > 1 && width + 1 >= cols;
                if !wrapped {
                    // Complete once the row is known to be over.
                    if ended.get(at_row).copied().unwrap_or(false) {
                        closed = true;
                        continue;
                    }
                    return None;
                }
                let next = at_row + 1;
                if !ended.get(at_row).copied().unwrap_or(false) || next >= rows.len() {
                    return None;
                }
                let nrow = trimmed(&rows[next]);
                let mut n = 0;
                while n < nrow.len() && token_char(nrow[n].ch) {
                    token.push(nrow[n].ch);
                    last = nrow[n].end;
                    n += 1;
                }
                if n == 0 {
                    // Nothing continues it: the wide row was the whole token.
                    closed = true;
                    continue;
                }
                if n < nrow.len() {
                    closed = true;
                    continue;
                }
                at_row = next;
                width = nrow.len();
            }
            if token.len() > 400 {
                continue;
            }
            if chat_token::valid_token(&token) {
                first = first.min(last);
                return Some(Found {
                    token,
                    span: first..last,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str =
        "sk-ant-oat01-AbC123_def-GHI456jklMNO789pqrSTUvwx-YZ0123456789abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn a_token_is_found_in_drawn_output_and_only_once_it_is_complete() {
        let colored = format!(
            "\x1b[2J\x1b[H✓ Created\r\n\r\nYour token:\r\n\r\n\x1b[1;32m{TOKEN}\x1b[0m\r\n\r\nStore it safely.\r\n"
        );
        let hit = find(&colored, 120).unwrap();
        assert_eq!(hit.token, TOKEN);
        // The span covers the token and nothing around it.
        assert_eq!(&colored[hit.span.clone()], TOKEN);
        assert!(
            !format!("{hit:?}").contains("sk-ant"),
            "never prints the token"
        );

        // Still arriving: a token with nothing after it is not taken.
        assert!(find(&format!("Your token:\r\n{TOKEN}"), 120).is_none());
        // A prefix with nothing real behind it, and plain text, give nothing.
        assert!(find("sk-ant-short\r\nand sk-ant-\r\n", 120).is_none());
        assert!(find("nothing here\r\n", 120).is_none());
        // A pay-as-you-go key is not a chat token.
        assert!(find("sk-ant-api03-AbC123_def-GHI456jklMNO789pqrSTU\r\n\r\n", 120).is_none());
    }

    #[test]
    fn a_token_wrapped_over_rows_is_joined_but_the_next_paragraph_is_not() {
        // A 60-wide terminal: the token fills one row and continues on the next.
        let (a, b) = TOKEN.split_at(60);
        let wrapped =
            format!("Your token:\r\n\r\n{a}\r\n{b}\r\n\r\nStore this token securely.\r\n");
        let hit = find(&wrapped, 60).unwrap();
        assert_eq!(hit.token, TOKEN);
        // The span reaches over the break inside the token, and stops at the token's end.
        let covered = &wrapped[hit.span.clone()];
        assert!(
            covered.starts_with("sk-ant-") && covered.ends_with(b),
            "{covered:?}"
        );

        // The same text on a wide terminal is two short rows: no joining with "Store ...".
        let narrow_run = format!("{TOKEN}\r\n\r\nStore this token securely.\r\n");
        assert_eq!(find(&narrow_run, 200).unwrap().token, TOKEN);

        // Cursor moves between rows count as line breaks too (a repainted screen).
        let painted =
            format!("\x1b[5;1HYour token:\x1b[7;1H{a}\x1b[8;1H{b}\x1b[10;1HStore it.\x1b[11;1H");
        assert_eq!(find(&painted, 60).unwrap().token, TOKEN);
        // The wrapped second row is not known to be over yet: wait.
        assert!(find(&format!("{a}\r\n{b}"), 60).is_none());
    }
}
