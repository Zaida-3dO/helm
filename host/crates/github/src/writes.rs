//! Checked writes: a top-level comment, a single review comment, a reply, resolve or unresolve, and
//! a review at the head the page showed. Each takes typed fields only, reads GitHub first, builds
//! GitHub's input from that read, and sends its write once. A refusal before the write sends
//! nothing; see `client` for how the write's own outcome is classed.
//!
//! Helm does not continue a review the user started on GitHub: while one is in progress, a single
//! comment, a reply or a new review is refused with `[ReviewPending]`, since GitHub would otherwise
//! file it into that unseen review.

use crate::client::{Answer, Client};
use crate::error::{Class, GhError, GhResult};
use crate::graphql::{self, is_node_id, is_repo_path, json_len, MAX_BODY_BYTES};
use crate::rest::{is_full_sha, is_name};
use crate::transport::{Method, Request};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommentArgs {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewCommentArgs {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub head: String,
    pub path: String,
    pub line: u64,
    pub side: String,
    #[serde(default)]
    pub start_line: Option<u64>,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplyArgs {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub thread_id: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveArgs {
    pub thread_id: String,
    pub resolve: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewArgs {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub head: String,
    pub event: String,
    pub body: String,
}

const MAX_LINE: u64 = 10_000_000;

pub fn check_pr(owner: &str, repo: &str, number: u64) -> GhResult<()> {
    if !is_name(owner) || !is_name(repo) {
        return Err(GhError::invalid("owner and repo must be GitHub names."));
    }
    if number == 0 || number > i32::MAX as u64 {
        return Err(GhError::invalid("number must be a pull request number."));
    }
    Ok(())
}

fn check_body(body: &str, required: bool) -> GhResult<()> {
    if required && body.trim().is_empty() {
        return Err(GhError::invalid("Write something first."));
    }
    if json_len(body) > MAX_BODY_BYTES {
        return Err(GhError::new(
            Class::TooLarge,
            "That text is over GitHub's 60,000-byte limit.",
        ));
    }
    Ok(())
}

fn check_node(id: &str) -> GhResult<()> {
    if is_node_id(id) {
        Ok(())
    } else {
        Err(GhError::invalid("That is not a GitHub thread id."))
    }
}

/// What a write needs to know about the pull request, read just before it.
#[derive(Debug)]
struct PrFacts {
    open: bool,
    head: String,
    pending: bool,
}

fn graphql_request(id: &str, variables: Value) -> GhResult<Request> {
    let doc = graphql::doc(id).expect("compiled document");
    Ok(Request {
        method: Method::Post,
        endpoint: "graphql".into(),
        body: Some(graphql::body(doc, Some(&variables))?),
    })
}

fn not_sent(a: &Answer) -> GhError {
    GhError::new(
        Class::Refused,
        format!("GitHub answered {}; nothing was sent.", a.status),
    )
}

fn read_pr(c: &Client, owner: &str, repo: &str, number: u64) -> GhResult<PrFacts> {
    let req = graphql_request(
        "pullRequestForWrite",
        json!({"owner": owner, "repo": repo, "number": number}),
    )?;
    let a = c.read(&req)?;
    if a.status != 200 {
        return Err(not_sent(&a));
    }
    let pr = &a.data["repository"]["pullRequest"];
    let (Some(state), Some(head)) = (pr["state"].as_str(), pr["headRefOid"].as_str()) else {
        return Err(GhError::new(
            Class::Refused,
            format!(
                "{owner}/{repo}#{number} is not a pull request Helm can see; nothing was sent."
            ),
        ));
    };
    Ok(PrFacts {
        open: state == "OPEN" && pr["merged"] != Value::Bool(true),
        head: head.to_owned(),
        pending: pr["pending"]["totalCount"].as_u64().unwrap_or(0) > 0,
    })
}

fn review_pending() -> GhError {
    GhError::new(
        Class::ReviewPending,
        "You have a review in progress on GitHub. Finish or discard it there first; nothing was sent.",
    )
}

fn head_moved() -> GhError {
    GhError::new(
        Class::HeadMoved,
        "The pull request has new commits since this view was loaded. Refresh to see the latest changes; nothing was sent.",
    )
}

fn rest_post(endpoint: String, body: Value) -> Request {
    Request {
        method: Method::Post,
        endpoint,
        body: Some(body),
    }
}

/// One top-level conversation comment. Allowed on closed and merged pull requests, as on GitHub.
pub fn comment(c: &Client, a: &CommentArgs) -> GhResult<Answer> {
    check_pr(&a.owner, &a.repo, a.number)?;
    check_body(&a.body, true)?;
    read_pr(c, &a.owner, &a.repo, a.number)?;
    c.write(&rest_post(
        format!("repos/{}/{}/issues/{}/comments", a.owner, a.repo, a.number),
        json!({"body": a.body}),
    ))
}

/// One published line or range comment on the head the page showed.
pub fn review_comment(c: &Client, a: &ReviewCommentArgs) -> GhResult<Answer> {
    check_pr(&a.owner, &a.repo, a.number)?;
    if !is_full_sha(&a.head) {
        return Err(GhError::invalid("head must be a full commit sha."));
    }
    if !is_repo_path(&a.path) {
        return Err(GhError::invalid("path must be a file in the repository."));
    }
    if !matches!(a.side.as_str(), "LEFT" | "RIGHT") {
        return Err(GhError::invalid("side must be LEFT or RIGHT."));
    }
    if a.line == 0 || a.line > MAX_LINE || a.start_line.is_some_and(|s| s == 0 || s >= a.line) {
        return Err(GhError::invalid(
            "line must be a line number, after startLine when one is given.",
        ));
    }
    check_body(&a.body, true)?;
    let pr = read_pr(c, &a.owner, &a.repo, a.number)?;
    if pr.head != a.head {
        return Err(head_moved());
    }
    if pr.pending {
        return Err(review_pending());
    }
    let mut body = json!({
        "body": a.body,
        "commit_id": a.head,
        "path": a.path,
        "line": a.line,
        "side": a.side,
    });
    if let Some(s) = a.start_line {
        body["start_line"] = json!(s);
        body["start_side"] = json!(a.side);
    }
    c.write(&rest_post(
        format!("repos/{}/{}/pulls/{}/comments", a.owner, a.repo, a.number),
        body,
    ))
}

/// The thread as GitHub has it: whether it is resolved and which pull request it is on.
struct ThreadFacts {
    resolved: bool,
    owner: String,
    repo: String,
    number: u64,
}

fn read_thread(c: &Client, thread_id: &str) -> GhResult<ThreadFacts> {
    let a = c.read(&graphql_request(
        "reviewThreadForWrite",
        json!({"threadId": thread_id}),
    )?)?;
    if a.status != 200 {
        return Err(not_sent(&a));
    }
    let n = &a.data["node"];
    let pr = &n["pullRequest"];
    match (
        n["__typename"].as_str(),
        n["isResolved"].as_bool(),
        pr["number"].as_u64(),
        pr["repository"]["owner"]["login"].as_str(),
        pr["repository"]["name"].as_str(),
    ) {
        (Some("PullRequestReviewThread"), Some(resolved), Some(number), Some(o), Some(r)) => {
            Ok(ThreadFacts {
                resolved,
                owner: o.to_owned(),
                repo: r.to_owned(),
                number,
            })
        }
        _ => Err(GhError::new(
            Class::Refused,
            "GitHub has no such review thread; nothing was sent.",
        )),
    }
}

/// A reply in an existing thread, posted at once.
pub fn review_reply(c: &Client, a: &ReplyArgs) -> GhResult<Answer> {
    check_pr(&a.owner, &a.repo, a.number)?;
    check_node(&a.thread_id)?;
    check_body(&a.body, true)?;
    let t = read_thread(c, &a.thread_id)?;
    if t.number != a.number
        || !t.owner.eq_ignore_ascii_case(&a.owner)
        || !t.repo.eq_ignore_ascii_case(&a.repo)
    {
        return Err(GhError::new(
            Class::Refused,
            "That thread is not on this pull request; nothing was sent.",
        ));
    }
    if read_pr(c, &a.owner, &a.repo, a.number)?.pending {
        return Err(review_pending());
    }
    c.mutate(
        &graphql_request(
            "addPullRequestReviewThreadReply",
            json!({"threadId": a.thread_id, "body": a.body}),
        )?,
        "addPullRequestReviewThreadReply",
    )
}

/// Resolve or unresolve one thread. Already in that state: answered from the read, no write
/// (`data.changed` is false).
pub fn review_resolve(c: &Client, a: &ResolveArgs) -> GhResult<Answer> {
    check_node(&a.thread_id)?;
    let t = read_thread(c, &a.thread_id)?;
    if t.resolved == a.resolve {
        return Ok(Answer {
            status: 200,
            rate_limit: None,
            next_page: None,
            data: json!({"thread": {"id": a.thread_id, "isResolved": t.resolved}, "changed": false}),
            errors: None,
        });
    }
    let id = if a.resolve {
        "resolveReviewThread"
    } else {
        "unresolveReviewThread"
    };
    let mut ans = c.mutate(&graphql_request(id, json!({"threadId": a.thread_id}))?, id)?;
    if let Value::Object(m) = &mut ans.data {
        m.insert("changed".into(), Value::Bool(true));
    }
    Ok(ans)
}

/// A new review at `head`, submitted at once: Comment, Approve or Request changes.
pub fn review(c: &Client, a: &ReviewArgs) -> GhResult<Answer> {
    check_pr(&a.owner, &a.repo, a.number)?;
    if !is_full_sha(&a.head) {
        return Err(GhError::invalid("head must be a full commit sha."));
    }
    let needs_text = match a.event.as_str() {
        "APPROVE" => false,
        "COMMENT" | "REQUEST_CHANGES" => true,
        _ => {
            return Err(GhError::invalid(
                "event must be COMMENT, APPROVE or REQUEST_CHANGES.",
            ))
        }
    };
    check_body(&a.body, needs_text)?;
    let pr = read_pr(c, &a.owner, &a.repo, a.number)?;
    if !pr.open {
        return Err(GhError::new(
            Class::Closed,
            "The pull request is closed; nothing was sent.",
        ));
    }
    if pr.head != a.head {
        return Err(head_moved());
    }
    if pr.pending {
        return Err(review_pending());
    }
    c.write(&rest_post(
        format!("repos/{}/{}/pulls/{}/reviews", a.owner, a.repo, a.number),
        json!({"commit_id": a.head, "body": a.body, "event": a.event}),
    ))
}
