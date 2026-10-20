//! A small, conservative shell reader for the landing guard. It splits a command line
//! into simple commands and words the way bash does: POSIX quoting, `$'...'` escapes, comments
//! only where bash starts one, and `$(…)` bodies that end where bash ends them. Anything it
//! cannot read literally (a variable or substitution in a word, unbalanced quotes, heredoc text
//! that bash would expand, a `#` inside `(( ))`, brace expansion, `eval`, `sh -c`, inline interpreter code) is either marked **computed** or
//! reported as **unreadable**, and the guard fails closed on it where work is at stake.

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
    /// The command's input is a heredoc or here-string written on the line.
    pub stdin_text: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Commands(Vec<Simple>),
    /// The line cannot be read: unbalanced quotes, a heredoc, or similar.
    Unreadable(&'static str),
}

type Res<T> = Result<T, &'static str>;

/// Why a line with brace expansion is unreadable: bash turns `{a,b}` and `{1..3}` into words the
/// reader never sees.
const BRACES: &str = "brace expansion, which the guard does not read";

/// Substitutions nested deeper than this are not read (it also bounds the reader's recursion).
const MAX_NESTING: usize = 64;

/// Split `line` into simple commands at `;`, `&&`, `||`, `|`, `&`, newlines and parentheses.
pub fn parse(line: &str) -> Parsed {
    let chars: Vec<char> = line.chars().collect();
    let mut r = Reader {
        s: &chars,
        i: 0,
        nesting: 0,
        heredocs: Vec::new(),
        arith_sub: false,
    };
    match r.list(false) {
        Ok(_) if !r.heredocs.is_empty() => Parsed::Unreadable("a heredoc without its end line"),
        Ok(c) => Parsed::Commands(c),
        Err(why) => Parsed::Unreadable(why),
    }
}

struct Reader<'a> {
    s: &'a [char],
    i: usize,
    nesting: usize,
    /// Heredocs whose text starts after the next newline.
    heredocs: Vec<Heredoc>,
    /// The substitution about to be read is `$((…))`.
    arith_sub: bool,
}

/// A `<<WORD` (or `<<-WORD`) waiting for its text.
struct Heredoc {
    delim: String,
    /// `<<-`: leading tabs are stripped from each line, the end line included.
    strip_tabs: bool,
    /// The delimiter was quoted, so the text is literal; otherwise bash expands `$` and
    /// backticks in it.
    quoted: bool,
    /// The substitution depth it was written at.
    nesting: usize,
}

/// The commands read so far, and the command and word being built.
#[derive(Default)]
struct Build {
    cmds: Vec<Simple>,
    cur: Simple,
    word: String,
    in_word: bool,
    computed_word: bool,
    /// Unquoted `{` still open in this word.
    braces: usize,
    /// The next word is a redirection's target, not an argument.
    target: bool,
}

impl Build {
    fn end_word(&mut self, nested: bool) -> Res<()> {
        self.braces = 0;
        if !self.in_word {
            return Ok(());
        }
        self.in_word = false;
        let computed = std::mem::take(&mut self.computed_word);
        let word = std::mem::take(&mut self.word);
        if std::mem::take(&mut self.target) {
            self.cur.computed |= computed;
            return Ok(());
        }
        if self.cur.words.is_empty() && !computed {
            if let Some((k, v)) = word.split_once('=') {
                if !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !k.starts_with(|c: char| c.is_ascii_digit())
                {
                    self.cur.assigns.push((k.to_owned(), v.to_owned()));
                    return Ok(());
                }
            }
        }
        // `{` and `}` standing alone are the group keywords (`f() { ...; }`, `function f {`):
        // they end the command. Where bash would read one as a plain word instead, splitting
        // there only shows the guard more commands.
        if word == "{" || word == "}" {
            self.push_command();
            return Ok(());
        }
        // A case pattern's `)` would end the substitution early here, where bash keeps reading.
        if nested && word == "case" {
            return Err("a case statement inside a substitution");
        }
        self.cur.computed |= computed;
        self.cur.words.push(word);
        Ok(())
    }

    fn push_command(&mut self) {
        // A command with no words can still run substitutions (`>$(…)` on its own).
        if !self.cur.words.is_empty() || !self.cur.assigns.is_empty() || !self.cur.subs.is_empty() {
            self.cmds.push(std::mem::take(&mut self.cur));
        }
    }

    fn end_command(&mut self, nested: bool) -> Res<()> {
        self.end_word(nested)?;
        if self.target {
            return Err("a redirection without a target");
        }
        self.push_command();
        Ok(())
    }
}

/// Where bash can start a comment: at the start of the line or right after a blank or an
/// operator. Anywhere else, `#` is part of a word (`a#b`, `{#`, `}#`, after a carriage return).
fn comment_may_follow(prev: Option<char>) -> bool {
    prev.is_none_or(|p| {
        matches!(
            p,
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
        )
    })
}

impl Reader<'_> {
    fn next(&mut self) -> Option<char> {
        let c = self.s.get(self.i).copied();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    /// Skip backslash-newline pairs. bash deletes them before it reads the line, so one can sit
    /// inside an operator (`<\<newline><`) or between `$` and what follows it.
    fn skip_joins(&mut self) {
        while self.peek() == Some('\\') && self.s.get(self.i + 1) == Some(&'\n') {
            self.i += 2;
        }
    }

    /// After `<<` or `<<-`: the delimiter word, and whether any of it was quoted. bash expands
    /// nothing in it, so a `$` or backtick (which it would keep as text) is refused rather than
    /// matched.
    fn heredoc_word(&mut self) -> Res<(String, bool)> {
        while self.peek().is_some_and(|x| matches!(x, ' ' | '\t')) {
            self.i += 1;
            self.skip_joins();
        }
        let mut delim = String::new();
        let mut quoted = false;
        while let Some(c) = self.peek() {
            if matches!(
                c,
                ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' | '(' | ')'
            ) {
                break;
            }
            self.i += 1;
            match c {
                '\'' | '"' => {
                    quoted = true;
                    loop {
                        match self.next() {
                            Some(x) if x == c => break,
                            Some('$' | '`' | '\\') if c == '"' => {
                                return Err("a heredoc delimiter the guard can't read")
                            }
                            Some(x) => delim.push(x),
                            None => return Err("an unclosed quote in a heredoc delimiter"),
                        }
                    }
                }
                '\\' => match self.next() {
                    Some('\n') => {}
                    Some(x) => {
                        quoted = true;
                        delim.push(x);
                    }
                    None => return Err("a trailing backslash"),
                },
                '$' | '`' => return Err("a heredoc delimiter the guard can't read"),
                c => delim.push(c),
            }
        }
        if delim.is_empty() {
            return Err("a heredoc without a delimiter");
        }
        Ok((delim, quoted))
    }

    /// After the newline that ends a line with heredocs: skip each one's text, up to its end
    /// line. The text is not commands. Unquoted text is expanded by bash, so a `$`, backtick or
    /// backslash in it makes the line unreadable.
    fn heredoc_text(&mut self) -> Res<()> {
        for h in std::mem::take(&mut self.heredocs) {
            if h.nesting != self.nesting {
                return Err("a heredoc whose text starts inside a substitution");
            }
            loop {
                if self.peek().is_none() {
                    return Err("a heredoc without its end line");
                }
                let start = self.i;
                while self.peek().is_some_and(|x| x != '\n') {
                    self.i += 1;
                }
                let line: String = self.s[start..self.i].iter().collect();
                self.next(); // the newline, if any
                let cmp = if h.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if cmp == h.delim {
                    break;
                }
                if !h.quoted && line.contains(['$', '`', '\\']) {
                    return Err("a heredoc whose text bash expands");
                }
            }
        }
        Ok(())
    }

    /// Read a command list. Inside a substitution (`nested`) it ends at the `)` that closes the
    /// substitution, which it consumes.
    fn list(&mut self, nested: bool) -> Res<Vec<Simple>> {
        let mut b = Build::default();
        let mut depth = 0usize;
        // The depths at which an arithmetic `((` opened and is still open.
        let mut arith: Vec<usize> = Vec::new();
        // The whole of this list is the body of a `$((…))`.
        let whole_arith = std::mem::take(&mut self.arith_sub);
        let mut prev: Option<char> = None;
        while let Some(c) = self.next() {
            let in_arith = whole_arith || !arith.is_empty();
            if matches!(c, '<' | '>' | '|' | '&' | '.' | '(') {
                // An operator can be split by a line continuation (`|\<newline>|`).
                self.skip_joins();
            }
            match c {
                '\'' => {
                    b.in_word = true;
                    loop {
                        match self.next() {
                            Some('\'') => break,
                            Some(x) => b.word.push(x),
                            None => return Err("an unclosed single quote"),
                        }
                    }
                }
                '"' => {
                    b.in_word = true;
                    self.double_quoted(&mut b)?;
                }
                '\\' => match self.next() {
                    // A line continuation is gone before bash reads the line: it neither ends a
                    // word nor counts as the character before a `#`.
                    Some('\n') => continue,
                    Some(x) => {
                        b.in_word = true;
                        b.word.push(x);
                    }
                    None => return Err("a trailing backslash"),
                },
                '$' => {
                    b.in_word = true;
                    self.dollar(&mut b, false)?;
                }
                '`' => {
                    b.in_word = true;
                    self.backticked(&mut b)?;
                }
                // bash reads `(( … ))` as one span with no comments in it, so a `#` there is
                // text, and the line goes on after it.
                '#' if !b.in_word && comment_may_follow(prev) && in_arith => {
                    return Err("a # inside (( ))");
                }
                '#' if !b.in_word && comment_may_follow(prev) => {
                    // A comment runs to the end of the line; the newline still ends the command.
                    while self.peek().is_some_and(|x| x != '\n') {
                        self.i += 1;
                    }
                }
                ';' | '\n' | '|' | '&' => {
                    b.end_command(nested)?;
                    if matches!(c, '|' | '&') && self.peek() == Some(c) {
                        self.i += 1;
                    }
                    if c == '\n' && !self.heredocs.is_empty() {
                        // bash reads `(( … ))` as one span, so heredoc text starts only after
                        // the newline that follows it.
                        if in_arith {
                            return Err("a heredoc whose text starts inside (( ))");
                        }
                        self.heredoc_text()?;
                    }
                }
                // Inside `(( … ))`, `<`, `>`, `<<` and `>>` compare and shift numbers: they are
                // not redirections or heredocs.
                '<' | '>' if in_arith => {
                    b.end_word(nested)?;
                    while self.peek().is_some_and(|x| matches!(x, '<' | '>' | '=')) {
                        self.i += 1;
                    }
                }
                '<' if self.peek() == Some('<') => {
                    b.end_word(nested)?;
                    self.i += 1;
                    self.skip_joins();
                    // The command reads text from the line itself, which a shell or interpreter
                    // would run.
                    b.cur.stdin_text = true;
                    if self.peek() == Some('<') {
                        // A here-string `<<< word`: the word is read (its substitutions run)
                        // but is not an argument.
                        self.i += 1;
                        b.target = true;
                    } else {
                        let strip_tabs = self.peek() == Some('-');
                        if strip_tabs {
                            self.i += 1;
                        }
                        let (delim, quoted) = self.heredoc_word()?;
                        self.heredocs.push(Heredoc {
                            delim,
                            strip_tabs,
                            quoted,
                            nesting: self.nesting,
                        });
                    }
                }
                '>' | '<' => {
                    // A redirection ends the word; its target is not an argument.
                    b.end_word(nested)?;
                    while self.peek().is_some_and(|x| matches!(x, '>' | '&' | '|')) {
                        self.i += 1;
                        self.skip_joins();
                    }
                    while self.peek().is_some_and(|x| matches!(x, ' ' | '\t')) {
                        self.i += 1;
                        self.skip_joins();
                    }
                    if self.peek() == Some('(') {
                        // A process substitution `<(…)` / `>(…)` runs a command.
                        self.i += 1;
                        let body = self.sub_body()?;
                        b.cur.subs.push(body);
                        // It is also a word (a file name), so a `#` right after it is part of
                        // that word, not a comment.
                        b.in_word = true;
                        b.computed_word = true;
                        b.word.push('$');
                    } else {
                        b.target = true;
                    }
                }
                '(' => {
                    b.end_command(nested)?;
                    if self.peek() == Some('(') {
                        // `((` opens an arithmetic command (also in `for ((`).
                        self.i += 1;
                        arith.push(depth);
                        depth += 1;
                    }
                    depth += 1;
                }
                ')' => {
                    b.end_command(nested)?;
                    if nested && depth == 0 {
                        return Ok(b.cmds);
                    }
                    depth = depth.saturating_sub(1);
                    while arith.last().is_some_and(|&d| depth <= d) {
                        arith.pop();
                    }
                }
                // Only space and tab are blanks to bash; a carriage return or a non-breaking
                // space is part of the word, so the reader keeps it there too.
                ' ' | '\t' => b.end_word(nested)?,
                c => {
                    b.in_word = true;
                    match c {
                        '{' => b.braces += 1,
                        '}' => b.braces = b.braces.saturating_sub(1),
                        ',' if b.braces > 0 => return Err(BRACES),
                        '.' if b.braces > 0 && self.peek() == Some('.') => return Err(BRACES),
                        _ => {}
                    }
                    b.word.push(c);
                }
            }
            prev = Some(c);
        }
        if nested {
            return Err("an unclosed command substitution");
        }
        b.end_command(nested)?;
        Ok(b.cmds)
    }

    /// After an opening `"`: read to the closing one.
    fn double_quoted(&mut self, b: &mut Build) -> Res<()> {
        loop {
            match self.next() {
                Some('"') => return Ok(()),
                Some('\\') => match self.next() {
                    Some('\n') => {}
                    Some(x) => b.word.push(x),
                    None => return Err("a trailing backslash"),
                },
                Some('$') => self.dollar(b, true)?,
                Some('`') => self.backticked(b)?,
                Some(x) => b.word.push(x),
                None => return Err("an unclosed double quote"),
            }
        }
    }

    /// After a `$`: a substitution, a parameter, or (outside double quotes) `$'...'`.
    fn dollar(&mut self, b: &mut Build, in_double: bool) -> Res<()> {
        self.skip_joins();
        match self.peek() {
            Some('\'') if !in_double => {
                self.i += 1;
                let text = self.ansi_c()?;
                b.word.push_str(&text);
                return Ok(());
            }
            Some('(') => {
                self.i += 1;
                // `$((…))` is arithmetic: its body is read with the `(( ))` rules.
                let arith = self.peek() == Some('(');
                self.arith_sub = arith;
                let body = self.sub_body()?;
                // Kept as `((…))`, so judging it on its own reads it as arithmetic too.
                b.cur
                    .subs
                    .push(if arith { format!("({body})") } else { body });
            }
            Some('{') => {
                self.i += 1;
                self.parameter()?;
            }
            // The old `$[ … ]` arithmetic: its `<<` and newlines follow other rules.
            Some('[') => return Err("a $[ ] arithmetic expansion"),
            // `$$`, `$#`, `$@`, `$1` and the like: one character, so `$$'x'` is not `$'x'`.
            Some(x)
                if matches!(x, '$' | '#' | '?' | '!' | '-' | '@' | '*') || x.is_ascii_digit() =>
            {
                self.i += 1;
            }
            _ => {}
        }
        b.computed_word = true;
        b.word.push('$');
        Ok(())
    }

    /// After `$(` or `<(`: the body up to the matching `)`, read with the same rules as the
    /// line, so a `)` in quotes or a comment does not end it early.
    fn sub_body(&mut self) -> Res<String> {
        if self.nesting >= MAX_NESTING {
            return Err("substitutions nested too deeply");
        }
        let start = self.i;
        self.nesting += 1;
        let read = self.list(true);
        self.nesting -= 1;
        read?;
        Ok(self.s[start..self.i - 1].iter().collect())
    }

    /// After a backtick: the body up to the next one. Inside backticks a backslash changes what
    /// bash reads (`` \` `` nests, `\$` unescapes), so one makes the line unreadable.
    fn backticked(&mut self, b: &mut Build) -> Res<()> {
        let mut body = String::new();
        loop {
            match self.next() {
                Some('`') => break,
                Some('\\') => return Err("a backslash inside backticks"),
                Some(x) => body.push(x),
                None => return Err("an unclosed backtick"),
            }
        }
        if !self.heredocs.is_empty() && body.contains('\n') {
            return Err("a heredoc whose text starts inside backticks");
        }
        if let Parsed::Unreadable(why) = parse(&body) {
            return Err(why);
        }
        b.cur.subs.push(body);
        b.computed_word = true;
        b.word.push('$');
        Ok(())
    }

    /// After `${`: skip to the matching `}`. Quotes, escapes, substitutions and bare `{` inside
    /// change where bash ends it, so any of them makes the line unreadable.
    fn parameter(&mut self) -> Res<()> {
        let mut depth = 1;
        loop {
            match self.next() {
                Some('}') => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some('$') if self.peek() == Some('{') => {
                    self.i += 1;
                    depth += 1;
                }
                Some('$') if self.peek() == Some('(') => {
                    return Err("a substitution inside ${...}")
                }
                Some('\'' | '"' | '`' | '\\' | '{') => {
                    return Err("quoting or a brace inside ${...}")
                }
                Some(_) => {}
                None => return Err("an unclosed ${...}"),
            }
        }
    }

    /// After `$'`: the string bash makes of it. A backslash escapes the next character, so `\'`
    /// does not end it. Escapes that make text the guard can't compare (a NUL, which ends the
    /// string early in bash, control characters, bytes that are not UTF-8) are unreadable.
    fn ansi_c(&mut self) -> Res<String> {
        const UNCLOSED: &str = "an unclosed $'...'";
        let mut out: Vec<u8> = Vec::new();
        let push = |out: &mut Vec<u8>, c: char| {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        };
        loop {
            let c = self.next().ok_or(UNCLOSED)?;
            if c == '\'' {
                break;
            }
            if c != '\\' {
                push(&mut out, c);
                continue;
            }
            let e = self.next().ok_or(UNCLOSED)?;
            match e {
                'a' => out.push(0x07),
                'b' => out.push(0x08),
                'e' | 'E' => out.push(0x1b),
                'f' => out.push(0x0c),
                'n' => out.push(b'\n'),
                'r' => out.push(b'\r'),
                't' => out.push(b'\t'),
                'v' => out.push(0x0b),
                '\\' | '\'' | '"' | '?' => push(&mut out, e),
                '0'..='7' => {
                    let mut v = e.to_digit(8).unwrap_or(0);
                    for _ in 0..2 {
                        match self.peek().and_then(|d| d.to_digit(8)) {
                            Some(d) => {
                                v = v * 8 + d;
                                self.i += 1;
                            }
                            None => break,
                        }
                    }
                    out.push((v & 0xff) as u8);
                }
                'x' if self.peek() == Some('{') => {
                    // `\x{HHH…}`: any number of hex digits, kept to one byte; the `}` may be
                    // left out. `\x{}` is a NUL.
                    self.i += 1;
                    let mut v: u32 = 0;
                    while let Some(d) = self.peek().and_then(|d| d.to_digit(16)) {
                        v = (v * 16 + d) & 0xff;
                        self.i += 1;
                    }
                    if self.peek() == Some('}') {
                        self.i += 1;
                    }
                    out.push(v as u8);
                }
                'x' | 'u' | 'U' => {
                    let max = match e {
                        'x' => 2,
                        'u' => 4,
                        _ => 8,
                    };
                    let mut v: u32 = 0;
                    let mut n = 0;
                    while n < max {
                        match self.peek().and_then(|d| d.to_digit(16)) {
                            Some(d) => {
                                v = v * 16 + d;
                                self.i += 1;
                                n += 1;
                            }
                            None => break,
                        }
                    }
                    if n == 0 {
                        // No digits: bash keeps the backslash and the letter.
                        out.push(b'\\');
                        push(&mut out, e);
                    } else if e == 'x' {
                        out.push(v as u8);
                    } else {
                        let ch = char::from_u32(v).ok_or("an escape that is not a character")?;
                        push(&mut out, ch);
                    }
                }
                'c' => return Err("a control-character escape in $'...'"),
                other => {
                    // Not an escape: bash keeps the backslash.
                    out.push(b'\\');
                    push(&mut out, other);
                }
            }
        }
        if out.contains(&0) {
            return Err("a NUL escape in $'...'");
        }
        String::from_utf8(out).map_err(|_| "an escape that makes bytes, not text, in $'...'")
    }
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

    fn subs(line: &str) -> Vec<String> {
        match parse(line) {
            Parsed::Commands(c) => c.into_iter().flat_map(|s| s.subs).collect(),
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
        assert_eq!(
            words("echo x > 'a b' && git push"),
            vec![vec!["echo", "x"], vec!["git", "push"]]
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
        for line in [
            "git push 'origin",
            "cat <<EOF\nx",
            "cat <<EOF\n$x\nEOF",
            "cat <<$X\nx\n$X",
            "echo $(cat <<EOF\n)\nx\nEOF",
            "(( 1 # ))",
            "echo $((1 # 2))",
            "echo $[1]",
            "cat <<EOF; (( 1 +\n2 ))\nx\nEOF",
            "echo $(git push",
            "echo >",
            "echo `a \\` b`",
            "echo ${x:-'}'}",
            "echo ${x:-{}",
            "echo $'abc",
        ] {
            assert!(matches!(parse(line), Parsed::Unreadable(_)), "{line}");
        }
    }

    #[test]
    fn comments_start_only_where_bash_starts_them() {
        assert_eq!(
            words("echo a#b {#c }#d $#"),
            vec![vec!["echo", "a#b", "{#c", "}#d", "$"]]
        );
        assert_eq!(
            words("echo x\r# y\ngit status"),
            vec![vec!["echo", "x\r#", "y"], vec!["git", "status"]]
        );
        assert_eq!(
            words("echo x;# y\n# it's\ngit status"),
            vec![vec!["echo", "x"], vec!["git", "status"]]
        );
    }

    #[test]
    fn parentheses_and_braces_are_read_as_bash_reads_them() {
        assert_eq!(
            words("(cd a && git push)"),
            vec![vec!["cd", "a"], vec!["git", "push"]]
        );
        assert_eq!(
            words("f() { git push; }; f"),
            vec![vec!["f"], vec!["git", "push"], vec!["f"]]
        );
        assert_eq!(
            words("git rev-parse HEAD@{1} ${x,} @{u}"),
            vec![vec!["git", "rev-parse", "HEAD@{1}", "$", "@{u}"]]
        );
        for line in [
            "echo {a,b}",
            "echo x{1..3}",
            "echo $(echo {a,b})",
            "echo \"$(echo {a,b})\"",
            "echo `echo {a,b}`",
        ] {
            assert_eq!(parse(line), Parsed::Unreadable(BRACES), "{line}");
        }
        for line in ["echo { a, b }", "echo '{a,b}' \"{a,b}\" {a\\,b} {}"] {
            assert!(matches!(parse(line), Parsed::Commands(_)), "{line}");
        }
    }

    #[test]
    fn substitutions_end_where_bash_ends_them() {
        assert_eq!(subs("echo \"$(echo ')')\""), vec!["echo ')'"]);
        assert_eq!(subs("echo $(echo \")\" # )\n)"), vec!["echo \")\" # )\n"]);
        assert_eq!(subs("echo $( (a) b )"), vec![" (a) b "]);
        assert_eq!(subs("echo $((1+2))"), vec!["((1+2))"]);
        assert_eq!(subs("cat <(git log)"), vec!["git log"]);
        assert_eq!(
            words("cat <(a)#b x\necho a |\\\n| echo b"),
            vec![
                vec!["cat", "$#b", "x"],
                vec!["echo", "a"],
                vec!["echo", "b"]
            ]
        );
        assert_eq!(
            parse("echo \"$(case x in a) b;; esac)\""),
            Parsed::Unreadable("a case statement inside a substitution")
        );
        let deep = format!("{}x{}", "$(".repeat(100), ")".repeat(100));
        assert!(matches!(parse(&deep), Parsed::Unreadable(_)));
    }

    #[test]
    fn ansi_c_strings_decode_like_bash() {
        assert_eq!(
            words(r"echo $'a\'b' $'\x70\x75sh' $'\160' $'é' $'\q' $'\x' $'t\tn'"),
            vec![vec![
                "echo",
                "a'b",
                "pus\u{68}",
                "p",
                "é",
                "\\q",
                "\\x",
                "t\tn"
            ]]
        );
        // `$$` is a parameter, so the quote after it is a plain single quote.
        assert_eq!(words("echo $$'a b'"), vec![vec!["echo", "$a b"]]);
        // Inside double quotes `$'` is not special.
        assert_eq!(words("echo \"$'x'\""), vec![vec!["echo", "$'x'"]]);
        for line in [
            r"echo $'\0'",
            r"echo $'\cA'",
            r"echo $'\xff'",
            r"echo $'\U110000'",
        ] {
            assert!(matches!(parse(line), Parsed::Unreadable(_)), "{line}");
        }
    }
}
