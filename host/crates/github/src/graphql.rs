//! GraphQL documents compiled into Helm. The renderer names a document by id and passes variables;
//! it never sends query text. An allowlist of operation names would not be enough, because a named
//! operation can wrap any mutation: only text compiled in here is ever sent.
//!
//! The variables object may hold only the variables the document declares, each of its declared
//! shape, and serialises to at most 64 KiB. Documents marked `native` are refused through
//! `github_graphql`: the review, reply and resolve mutations, and the reads the checked writes make
//! before them, are built only by the host from what it read itself.

use crate::error::{GhError, GhResult};
use serde_json::{Map, Value};

pub const MAX_VARIABLES_BYTES: usize = 64 * 1024;
/// The most a comment or review body may be (as JSON encodes it, without the quotes).
pub const MAX_BODY_BYTES: usize = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Var {
    /// An owner or repository name.
    Name,
    /// A pull request number (GraphQL `Int`).
    Number,
    /// A paging cursor; may be null or absent.
    Cursor,
    /// A GitHub node id.
    NodeId,
    /// A repository file path.
    Path,
    /// Search text.
    Query,
    /// 1–100.
    First,
    /// A comment body.
    Body,
}

pub struct Doc {
    pub id: &'static str,
    pub text: &'static str,
    /// Name, shape and whether it is required.
    pub vars: &'static [(&'static str, Var, bool)],
    /// Built only by the host; refused through `github_graphql`.
    pub native: bool,
}

const PR_VARS: &[(&str, Var, bool)] = &[
    ("owner", Var::Name, true),
    ("repo", Var::Name, true),
    ("number", Var::Number, true),
];
const PR_PAGED_VARS: &[(&str, Var, bool)] = &[
    ("owner", Var::Name, true),
    ("repo", Var::Name, true),
    ("number", Var::Number, true),
    ("after", Var::Cursor, false),
];

pub const DOCS: &[Doc] = &[
    Doc {
        id: "viewer",
        text: "query { viewer { login name avatarUrl } }",
        vars: &[],
        native: false,
    },
    Doc {
        id: "pullRequestSummary",
        text: r#"query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      id number title url body state isDraft merged mergedAt closedAt createdAt updatedAt
      author { login }
      baseRefName baseRefOid headRefName headRefOid
      headRepository { nameWithOwner }
      mergeable reviewDecision additions deletions changedFiles
      commits { totalCount }
      comments { totalCount }
      reviewThreads { totalCount }
      labels(first: 20) { nodes { name color } }
      assignees(first: 20) { nodes { login } }
      reviewRequests(first: 20) { nodes { requestedReviewer { __typename ... on User { login } ... on Team { name } } } }
      latestReviews(first: 20) { nodes { author { login } state submittedAt } }
      lastCommit: commits(last: 1) { nodes { commit { oid statusCheckRollup { state } } } }
    }
  }
}"#,
        vars: PR_VARS,
        native: false,
    },
    Doc {
        id: "pullRequestNodeId",
        text: r#"query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) { pullRequest(number: $number) { id } }
}"#,
        vars: PR_VARS,
        native: false,
    },
    Doc {
        id: "pullRequestFileViewedStates",
        text: r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      files(first: 100, after: $after) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes { path viewerViewedState }
      }
    }
  }
}"#,
        vars: PR_PAGED_VARS,
        native: false,
    },
    Doc {
        id: "pullRequestCommits",
        text: r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      commits(first: 100, after: $after) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes {
          commit {
            oid abbreviatedOid messageHeadline messageBody committedDate authoredDate url
            author { name user { login } }
            signature { isValid }
            statusCheckRollup { state }
          }
        }
      }
    }
  }
}"#,
        vars: PR_PAGED_VARS,
        native: false,
    },
    Doc {
        id: "pullRequestTimeline",
        text: r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      updatedAt
      timelineItems(first: 50, after: $after, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, PULL_REQUEST_COMMIT, HEAD_REF_FORCE_PUSHED_EVENT, MERGED_EVENT, CLOSED_EVENT, REOPENED_EVENT, REVIEW_REQUESTED_EVENT, READY_FOR_REVIEW_EVENT, CONVERT_TO_DRAFT_EVENT, LABELED_EVENT, UNLABELED_EVENT]) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes {
          __typename
          ... on IssueComment { id databaseId author { login } body createdAt updatedAt url }
          ... on PullRequestReview { id author { login } state body submittedAt url }
          ... on PullRequestCommit { commit { oid abbreviatedOid messageHeadline committedDate author { name user { login } } } }
          ... on HeadRefForcePushedEvent { actor { login } createdAt beforeCommit { abbreviatedOid } afterCommit { abbreviatedOid } }
          ... on MergedEvent { actor { login } createdAt commit { abbreviatedOid } }
          ... on ClosedEvent { actor { login } createdAt }
          ... on ReopenedEvent { actor { login } createdAt }
          ... on ReviewRequestedEvent { actor { login } createdAt requestedReviewer { __typename ... on User { login } ... on Team { name } } }
          ... on ReadyForReviewEvent { actor { login } createdAt }
          ... on ConvertToDraftEvent { actor { login } createdAt }
          ... on LabeledEvent { actor { login } createdAt label { name color } }
          ... on UnlabeledEvent { actor { login } createdAt label { name color } }
        }
      }
    }
  }
}"#,
        vars: PR_PAGED_VARS,
        native: false,
    },
    Doc {
        id: "reviewThreads",
        text: r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 50, after: $after) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated isCollapsed path line originalLine startLine originalStartLine
          diffSide startDiffSide subjectType viewerCanResolve viewerCanUnresolve viewerCanReply
          resolvedBy { login }
          comments(first: 50) {
            totalCount
            nodes {
              id databaseId author { login } body createdAt updatedAt url outdated diffHunk
              commit { oid } originalCommit { oid }
              pullRequestReview { id state }
            }
          }
        }
      }
    }
  }
}"#,
        vars: PR_PAGED_VARS,
        native: false,
    },
    Doc {
        id: "statusCheckRollup",
        text: r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      commits(last: 1) {
        nodes {
          commit {
            oid
            statusCheckRollup {
              state
              contexts(first: 100, after: $after) {
                totalCount
                pageInfo { hasNextPage endCursor }
                nodes {
                  __typename
                  ... on CheckRun { name status conclusion detailsUrl startedAt completedAt checkSuite { app { name } workflowRun { workflow { name } } } }
                  ... on StatusContext { context state description targetUrl createdAt }
                }
              }
            }
          }
        }
      }
    }
  }
}"#,
        vars: PR_PAGED_VARS,
        native: false,
    },
    Doc {
        id: "searchPullRequests",
        text: r#"query($q: String!, $first: Int!) {
  search(query: $q, type: ISSUE, first: $first) {
    issueCount
    nodes {
      __typename
      ... on PullRequest { number title url state isDraft merged updatedAt author { login } repository { nameWithOwner } }
    }
  }
}"#,
        vars: &[("q", Var::Query, true), ("first", Var::First, true)],
        native: false,
    },
    Doc {
        id: "markFileAsViewed",
        text: r#"mutation($pullRequestId: ID!, $path: String!) {
  markFileAsViewed(input: {pullRequestId: $pullRequestId, path: $path}) { pullRequest { id } }
}"#,
        vars: &[
            ("pullRequestId", Var::NodeId, true),
            ("path", Var::Path, true),
        ],
        native: false,
    },
    Doc {
        id: "unmarkFileAsViewed",
        text: r#"mutation($pullRequestId: ID!, $path: String!) {
  unmarkFileAsViewed(input: {pullRequestId: $pullRequestId, path: $path}) { pullRequest { id } }
}"#,
        vars: &[
            ("pullRequestId", Var::NodeId, true),
            ("path", Var::Path, true),
        ],
        native: false,
    },
    // ---- native only ----
    Doc {
        id: "pullRequestForWrite",
        text: r#"query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      id state merged headRefOid
      pending: reviews(states: PENDING, first: 1) { totalCount }
    }
  }
}"#,
        vars: PR_VARS,
        native: true,
    },
    Doc {
        id: "reviewThreadForWrite",
        text: r#"query($threadId: ID!) {
  node(id: $threadId) {
    __typename
    ... on PullRequestReviewThread { id isResolved pullRequest { number repository { name owner { login } } } }
  }
}"#,
        vars: &[("threadId", Var::NodeId, true)],
        native: true,
    },
    Doc {
        id: "addPullRequestReviewThreadReply",
        text: r#"mutation($threadId: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $threadId, body: $body}) {
    comment { id databaseId url body createdAt author { login } }
  }
}"#,
        vars: &[("threadId", Var::NodeId, true), ("body", Var::Body, true)],
        native: true,
    },
    Doc {
        id: "resolveReviewThread",
        text: r#"mutation($threadId: ID!) {
  resolveReviewThread(input: {threadId: $threadId}) { thread { id isResolved } }
}"#,
        vars: &[("threadId", Var::NodeId, true)],
        native: true,
    },
    Doc {
        id: "unresolveReviewThread",
        text: r#"mutation($threadId: ID!) {
  unresolveReviewThread(input: {threadId: $threadId}) { thread { id isResolved } }
}"#,
        vars: &[("threadId", Var::NodeId, true)],
        native: true,
    },
];

pub fn doc(id: &str) -> Option<&'static Doc> {
    DOCS.iter().find(|d| d.id == id)
}

/// A GitHub node id: 1–200 of letters, digits, `_`, `-`, `=`, `:`.
pub fn is_node_id(s: &str) -> bool {
    (1..=200).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'=' | b':'))
}

/// A repository file path as GitHub names it: no leading `/`, no `.` or `..` part, no backslash or
/// control character, at most 4096 bytes.
pub fn is_repo_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 4096
        && !s.starts_with('/')
        && !s.ends_with('/')
        && !s.chars().any(|c| c == '\\' || c.is_control())
        && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// How many bytes `s` takes as JSON encodes it, without the quotes.
pub fn json_len(s: &str) -> usize {
    serde_json::to_string(s)
        .map(|j| j.len() - 2)
        .unwrap_or(usize::MAX)
}

fn check_var(name: &str, kind: Var, v: &Value) -> GhResult<()> {
    let bad = || GhError::invalid(format!("Variable {name} is not usable."));
    let ok = match kind {
        Var::Name => v.as_str().is_some_and(crate::rest::is_name),
        Var::Number => v
            .as_i64()
            .is_some_and(|n| (1..=i32::MAX as i64).contains(&n)),
        Var::Cursor => {
            v.is_null()
                || v.as_str().is_some_and(|s| {
                    (1..=200).contains(&s.len()) && !s.chars().any(char::is_control)
                })
        }
        Var::NodeId => v.as_str().is_some_and(is_node_id),
        Var::Path => v.as_str().is_some_and(is_repo_path),
        Var::Query => v
            .as_str()
            .is_some_and(|s| s.len() <= 512 && !s.chars().any(char::is_control)),
        Var::First => v.as_i64().is_some_and(|n| (1..=100).contains(&n)),
        Var::Body => v.as_str().is_some_and(|s| json_len(s) <= MAX_BODY_BYTES),
    };
    if ok {
        Ok(())
    } else {
        Err(bad())
    }
}

/// Check `variables` against the document and build the request body.
pub fn body(doc: &Doc, variables: Option<&Value>) -> GhResult<Value> {
    let vars = match variables {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(GhError::invalid("variables must be an object.")),
    };
    for k in vars.keys() {
        if !doc.vars.iter().any(|(n, _, _)| n == k) {
            return Err(GhError::invalid(format!(
                "{} does not declare a variable {k}.",
                doc.id
            )));
        }
    }
    for (name, kind, required) in doc.vars {
        match vars.get(*name) {
            Some(v) => check_var(name, *kind, v)?,
            None if *required => {
                return Err(GhError::invalid(format!(
                    "{} needs the variable {name}.",
                    doc.id
                )))
            }
            None => {}
        }
    }
    let vars = Value::Object(vars);
    if serde_json::to_vec(&vars).map_or(usize::MAX, |b| b.len()) > MAX_VARIABLES_BYTES {
        return Err(GhError::new(
            crate::error::Class::TooLarge,
            "The variables are over 64 KiB.",
        ));
    }
    Ok(serde_json::json!({"query": doc.text, "variables": vars}))
}

/// Whether the document is a mutation.
pub fn is_mutation(doc: &Doc) -> bool {
    doc.text.trim_start().starts_with("mutation")
}
