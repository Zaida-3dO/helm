//! Reading a git command line the way git does, far enough to tell a worktree command from
//! everything else. Only `worktree add` and `worktree remove` are recognised; every other command
//! line is "not ours" and is passed to the real git untouched.
//!
//! Not handled, on purpose: git aliases (`git wt add` when `alias.wt` is `worktree`) and
//! autocorrected spellings. An unknown global option makes the parser look for `worktree add` or
//! `worktree remove` anywhere in the arguments (fail closed).

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Anything else.
    Other,
    WorktreeAdd {
        path: Option<String>,
    },
    WorktreeRemove {
        path: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    /// The global options before the verb, exactly as given, to replay on a query.
    pub globals: Vec<String>,
    /// The `-C <dir>` values, in order.
    pub dirs: Vec<String>,
}

/// Global options that take their value as the next argument.
const WITH_VALUE: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--super-prefix",
    "--attr-source",
];
/// Global options that stand alone.
const FLAGS: &[&str] = &[
    "--no-pager",
    "-p",
    "--paginate",
    "-P",
    "--no-replace-objects",
    "--bare",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-optional-locks",
    "--no-lazy-fetch",
    "--no-advice",
    "-v",
    "--version",
    "-h",
    "--help",
    "--exec-path",
    "--html-path",
    "--man-path",
    "--info-path",
];

/// `worktree add` options that take a value in the next argument.
const ADD_WITH_VALUE: &[&str] = &["-b", "-B", "--reason"];

fn first_positional(args: &[String], with_value: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return args.get(i + 1).cloned();
        }
        if with_value.contains(&a.as_str()) {
            i += 2;
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            i += 1;
            continue;
        }
        return Some(a.clone());
    }
    None
}

fn command_from(verb: &str, rest: &[String]) -> Command {
    if verb != "worktree" {
        return Command::Other;
    }
    match rest.first().map(String::as_str) {
        Some("add") => Command::WorktreeAdd {
            path: first_positional(&rest[1..], ADD_WITH_VALUE),
        },
        Some("remove" | "rm") => Command::WorktreeRemove {
            path: first_positional(&rest[1..], &[]),
        },
        _ => Command::Other,
    }
}

pub fn parse(args: &[String]) -> Parsed {
    let mut globals = Vec::new();
    let mut dirs = Vec::new();
    let mut i = 0;
    let mut unknown = false;
    while i < args.len() {
        let a = args[i].as_str();
        if WITH_VALUE.contains(&a) {
            globals.push(args[i].clone());
            if let Some(v) = args.get(i + 1) {
                globals.push(v.clone());
                if a == "-C" {
                    dirs.push(v.clone());
                }
            }
            i += 2;
        } else if FLAGS.contains(&a)
            || [
                "--git-dir=",
                "--work-tree=",
                "--namespace=",
                "--config-env=",
                "--exec-path=",
                "--super-prefix=",
                "--attr-source=",
            ]
            .iter()
            .any(|p| a.starts_with(p))
        {
            globals.push(args[i].clone());
            i += 1;
        } else if a.starts_with('-') && a.len() > 1 {
            // Not a global option we know: it may take a value or not. Stop reading globals
            // and look for the pair anywhere.
            unknown = true;
            break;
        } else {
            break;
        }
    }
    let command = if unknown {
        args.windows(2)
            .position(|w| w[0] == "worktree" && matches!(w[1].as_str(), "add" | "remove" | "rm"))
            .map(|p| command_from("worktree", &args[p + 1..]))
            .unwrap_or(Command::Other)
    } else {
        match args.get(i) {
            Some(verb) => command_from(verb, &args[i + 1..]),
            None => Command::Other,
        }
    };
    Parsed {
        command,
        globals,
        dirs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Parsed {
        parse(&s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
    }

    #[test]
    fn only_worktree_add_and_remove_are_recognised() {
        use Command::*;
        let add = |path: Option<&str>| WorktreeAdd {
            path: path.map(Into::into),
        };
        let rm = |path: Option<&str>| WorktreeRemove {
            path: path.map(Into::into),
        };
        for (line, want) in [
            ("status", Other),
            ("", Other),
            ("worktree list", Other),
            ("worktree prune", Other),
            ("worktree add ../x", add(Some("../x"))),
            ("worktree add -b feat C:/w/h/x main", add(Some("C:/w/h/x"))),
            ("worktree add --detach -f C:/w/x", add(Some("C:/w/x"))),
            ("worktree add --reason why -B b C:/w/x", add(Some("C:/w/x"))),
            ("worktree add -- -odd", add(Some("-odd"))),
            ("worktree add", add(None)),
            ("-C C:/repo worktree add C:/w/x", add(Some("C:/w/x"))),
            (
                "--no-pager -c core.x=1 worktree remove C:/w/x",
                rm(Some("C:/w/x")),
            ),
            (
                "--git-dir=C:/r/.git worktree rm -f C:/w/x",
                rm(Some("C:/w/x")),
            ),
            ("worktree remove --force C:/w/x", rm(Some("C:/w/x"))),
            // Unknown global option: fail closed, look for the pair.
            ("--future-flag worktree add C:/w/x", add(Some("C:/w/x"))),
            ("--future-flag status", Other),
            // A path that merely says "worktree" is not the verb.
            ("add worktree add", Other),
        ] {
            assert_eq!(p(line).command, want, "{line:?}");
        }
        let g = p("-C a -C b -c x=y --no-pager worktree add z");
        assert_eq!(g.dirs, ["a", "b"]);
        assert_eq!(g.globals, ["-C", "a", "-C", "b", "-c", "x=y", "--no-pager"]);
    }
}
