//! A small, conservative shell reader for the landing guard. It splits a command line
//! into simple commands and words with POSIX quoting. Anything it cannot read literally (command
//! substitution, a variable in a word, unbalanced quotes, `eval`, `sh -c`, inline interpreter
//! code) is reported as **unreadable**, and the guard fails closed on it where work is at stake.

/// One simple command: leading `NAME=value` assignments, then its words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Simple {
    pub assigns: Vec<(String, String)>,
    pub words: Vec<String>,
    /// A word held a computed part (`$x`, `$(…)`, backticks), so its literal text is unknown.
    pub computed: bool,
    /// The bodies of command and process substitutions in this command (`$(…)`, backticks,
    /// `<(…)`, `>(…)`), which the guard judges as commands of their own.
    pub subs: Vec<String>,
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// The text up to the `)` matching an already-consumed `(`, or `None` if it never closes.
fn balanced(chars: &mut Chars<'_>) -> Option<String> {
    let mut depth = 1;
    let mut out = String::new();
    for c in chars.by_ref() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(out);
                }
            }
            _ => {}
        }
        out.push(c);
    }
    None
}

/// The text up to the next backtick, or `None` if it never closes.
fn backticked(chars: &mut Chars<'_>) -> Option<String> {
    let mut out = String::new();
    for c in chars.by_ref() {
        if c == '`' {
            return Some(out);
        }
        out.push(c);
    }
    None
}

/// After a `$` or a backtick: record a substitution body, if one starts here.
fn substitution(
    c: char,
    chars: &mut Chars<'_>,
    subs: &mut Vec<String>,
) -> Result<(), &'static str> {
    if c == '`' {
        subs.push(backticked(chars).ok_or("an unclosed backtick")?);
    } else if chars.peek() == Some(&'(') {
        chars.next();
        subs.push(balanced(chars).ok_or("an unclosed command substitution")?);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Commands(Vec<Simple>),
    /// The line cannot be read: unbalanced quotes, a heredoc, or similar.
    Unreadable(&'static str),
}

/// Split `line` into simple commands at `;`, `&&`, `||`, `|`, `&` and newlines.
pub fn parse(line: &str) -> Parsed {
    let mut cmds = Vec::new();
    let mut cur = Simple::default();
    let mut word = String::new();
    let mut in_word = false;
    let mut computed_word = false;
    let mut chars = line.chars().peekable();
    let finish_word =
        |cur: &mut Simple, word: &mut String, in_word: &mut bool, computed: &mut bool| {
            if *in_word {
                if cur.words.is_empty() && !*computed {
                    if let Some((k, v)) = word.split_once('=') {
                        if !k.is_empty()
                            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && !k.starts_with(|c: char| c.is_ascii_digit())
                        {
                            cur.assigns.push((k.to_owned(), v.to_owned()));
                            word.clear();
                            *in_word = false;
                            return;
                        }
                    }
                }
                if *computed {
                    cur.computed = true;
                }
                cur.words.push(std::mem::take(word));
                *in_word = false;
                *computed = false;
            }
        };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => word.push(x),
                        None => return Parsed::Unreadable("an unclosed single quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(x) => word.push(x),
                            None => return Parsed::Unreadable("a trailing backslash"),
                        },
                        Some(x @ ('$' | '`')) => {
                            computed_word = true;
                            word.push('$');
                            if let Err(why) = substitution(x, &mut chars, &mut cur.subs) {
                                return Parsed::Unreadable(why);
                            }
                        }
                        Some(x) => word.push(x),
                        None => return Parsed::Unreadable("an unclosed double quote"),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(x) => {
                    in_word = true;
                    word.push(x);
                }
                None => return Parsed::Unreadable("a trailing backslash"),
            },
            '$' | '`' => {
                in_word = true;
                computed_word = true;
                word.push('$');
                if let Err(why) = substitution(c, &mut chars, &mut cur.subs) {
                    return Parsed::Unreadable(why);
                }
            }
            '#' if !in_word => {
                // A comment runs to the end of the line.
                for x in chars.by_ref() {
                    if x == '\n' {
                        break;
                    }
                }
                finish_word(&mut cur, &mut word, &mut in_word, &mut computed_word);
                if !cur.words.is_empty() || !cur.assigns.is_empty() {
                    cmds.push(std::mem::take(&mut cur));
                }
            }
            ';' | '\n' | '|' | '&' => {
                finish_word(&mut cur, &mut word, &mut in_word, &mut computed_word);
                if !cur.words.is_empty() || !cur.assigns.is_empty() {
                    cmds.push(std::mem::take(&mut cur));
                }
                if matches!(c, '|' | '&') && chars.peek() == Some(&c) {
                    chars.next();
                }
            }
            '<' if chars.peek() == Some(&'<') => return Parsed::Unreadable("a heredoc"),
            '>' | '<' => {
                // A redirection ends the word; its target is not an argument.
                finish_word(&mut cur, &mut word, &mut in_word, &mut computed_word);
                while chars.peek().is_some_and(|x| matches!(x, '>' | '&')) {
                    chars.next();
                }
                while chars.peek().is_some_and(|x| *x == ' ') {
                    chars.next();
                }
                // A process substitution `<(…)` / `>(…)` or a computed target `>$(…)` runs a
                // command: keep its body for the guard.
                if chars.peek() == Some(&'(') {
                    chars.next();
                    match balanced(&mut chars) {
                        Some(b) => cur.subs.push(b),
                        None => return Parsed::Unreadable("an unclosed process substitution"),
                    }
                }
                while let Some(&x) = chars.peek() {
                    if x.is_whitespace() || matches!(x, ';' | '|' | '&') {
                        break;
                    }
                    chars.next();
                    if matches!(x, '$' | '`') {
                        cur.computed = true;
                        if let Err(why) = substitution(x, &mut chars, &mut cur.subs) {
                            return Parsed::Unreadable(why);
                        }
                    }
                }
            }
            '(' | ')' | '{' | '}' if !in_word => {
                finish_word(&mut cur, &mut word, &mut in_word, &mut computed_word);
                if !cur.words.is_empty() || !cur.assigns.is_empty() {
                    cmds.push(std::mem::take(&mut cur));
                }
            }
            c if c.is_whitespace() => {
                finish_word(&mut cur, &mut word, &mut in_word, &mut computed_word)
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    finish_word(&mut cur, &mut word, &mut in_word, &mut computed_word);
    if !cur.words.is_empty() || !cur.assigns.is_empty() {
        cmds.push(cur);
    }
    Parsed::Commands(cmds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<Vec<String>> {
        match parse(line) {
            Parsed::Commands(c) => c.into_iter().map(|s| s.words).collect(),
            Parsed::Unreadable(w) => panic!("unreadable: {w}"),
        }
    }

    #[test]
    fn commands_split_at_operators_and_keep_quoted_words_whole() {
        assert_eq!(
            words("cd 'a b' && git push origin \"main\"; echo x | tee y"),
            vec![
                vec!["cd", "a b"],
                vec!["git", "push", "origin", "main"],
                vec!["echo", "x"],
                vec!["tee", "y"]
            ]
        );
        assert_eq!(words("git status # git push"), vec![vec!["git", "status"]]);
        assert_eq!(
            words("git push origin HEAD:main>/dev/null 2>&1"),
            vec![vec!["git", "push", "origin", "HEAD:main", "2"]]
        );
    }

    #[test]
    fn assignments_and_computed_words_are_marked() {
        let Parsed::Commands(c) = parse("GIT_DIR=/x git push") else {
            panic!()
        };
        assert_eq!(c[0].assigns, [("GIT_DIR".to_owned(), "/x".to_owned())]);
        assert_eq!(c[0].words, ["git", "push"]);
        let Parsed::Commands(c) = parse("git $(echo push) origin") else {
            panic!()
        };
        assert!(c[0].computed);
        let Parsed::Commands(c) = parse("git push origin \"$BRANCH\"") else {
            panic!()
        };
        assert!(c[0].computed);
    }

    #[test]
    fn broken_lines_are_unreadable() {
        assert!(matches!(parse("git push 'origin"), Parsed::Unreadable(_)));
        assert!(matches!(parse("cat <<EOF\nx\nEOF"), Parsed::Unreadable(_)));
    }
}
