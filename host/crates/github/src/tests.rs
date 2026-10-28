use super::*;
use crate::transport::{gh_argv, parse_included, Response, TransportError};
use helm_ipc::RecordingSink;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

// ---- a scripted transport ---------------------------------------------------------------

#[derive(Default)]
struct Script {
    answers: Mutex<VecDeque<Result<Response, TransportError>>>,
    sent: Mutex<Vec<Request>>,
}

impl Script {
    fn new(answers: Vec<Result<Response, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            sent: Mutex::default(),
        })
    }
    fn sent(&self) -> Vec<Request> {
        self.sent.lock().unwrap().clone()
    }
}

impl Transport for Script {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        self.sent.lock().unwrap().push(req.clone());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("the script ran out: an unexpected request was sent")
    }
}

const NOW: i64 = 1_800_000_000_000;

fn client_at(script: &Arc<Script>, now: &Arc<AtomicI64>) -> GitHub {
    let now = now.clone();
    GitHub {
        client: Client::new(script.clone(), Arc::new(move || now.load(Ordering::SeqCst))),
    }
}

fn gh(script: &Arc<Script>) -> GitHub {
    client_at(script, &Arc::new(AtomicI64::new(NOW)))
}

fn res(status: u16, headers: &[(&str, &str)], body: Value) -> Result<Response, TransportError> {
    Ok(Response {
        status,
        headers: headers
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect(),
        body: if body.is_null() {
            String::new()
        } else {
            body.to_string()
        },
    })
}

fn ok(body: Value) -> Result<Response, TransportError> {
    res(200, &[], body)
}

fn class(r: &GhResult<Answer>) -> Option<Class> {
    r.as_ref().err().map(|e| e.class)
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

// ---- REST allowlist -----------------------------------------------------------------------

#[test]
fn rest_reads_only_allowlisted_routes() {
    let cases: &[(&str, Value, Option<&str>)] = &[
        ("/user", Value::Null, Some("user")),
        ("/repos/o/r", Value::Null, Some("repos/o/r")),
        (
            "/repos/o-1/r.js/pulls/12/files",
            json!({"per_page": 100, "page": "3"}),
            Some("repos/o-1/r.js/pulls/12/files?page=3&per_page=100"),
        ),
        (
            "/repos/o/r/pulls/12/commits",
            Value::Null,
            Some("repos/o/r/pulls/12/commits"),
        ),
        (
            "/repos/o/r/pulls/12/comments",
            Value::Null,
            Some("repos/o/r/pulls/12/comments"),
        ),
        (
            "/repos/o/r/pulls/12/reviews",
            json!({"page": 1000}),
            Some("repos/o/r/pulls/12/reviews?page=1000"),
        ),
        (
            "/repos/o/r/issues/12/comments",
            Value::Null,
            Some("repos/o/r/issues/12/comments"),
        ),
        ("/repos/o/r/labels", Value::Null, Some("repos/o/r/labels")),
        (
            "/repos/o/r/compare/abc1234...def5678",
            Value::Null,
            Some("repos/o/r/compare/abc1234...def5678"),
        ),
        (
            "/repos/o/r/commits/abc1234/check-runs",
            Value::Null,
            Some("repos/o/r/commits/abc1234/check-runs"),
        ),
        (
            "/repos/o/r/commits/abc1234/status",
            Value::Null,
            Some("repos/o/r/commits/abc1234/status"),
        ),
        (
            "/repos/o/r/contents/src/my%20file.rs",
            json!({"ref": "feature/x-1"}),
            Some("repos/o/r/contents/src/my%20file.rs?ref=feature/x-1"),
        ),
        // Refused: shapes, routes, queries.
        ("user", Value::Null, None),
        ("/user/", Value::Null, None),
        ("/user?x=1", Value::Null, None),
        ("/repos/o/r/pulls/012", Value::Null, None),
        ("/repos/o/r/pulls/12345678901", Value::Null, None),
        ("/repos/../r", Value::Null, None),
        ("/repos/o/r/pulls/12/merge", Value::Null, None),
        ("/repos/o/r/issues/12/labels", Value::Null, None),
        ("/repos/{owner}/r", Value::Null, None),
        ("/repos/o/r/compare/main...dev", Value::Null, None),
        ("/repos/o/r/contents/a/%2e%2e/b", Value::Null, None),
        ("/repos/o/r/contents/a%2Fb", Value::Null, None),
        ("/repos/o/r/contents/a%5Cb", Value::Null, None),
        ("/repos/o/r/contents/a b", Value::Null, None),
        ("/repos/o/r/contents", Value::Null, None),
        ("/repos/o/r/pulls/12", json!({"page": 2}), None),
        ("/repos/o/r/pulls/12/files", json!({"page": 1001}), None),
        ("/repos/o/r/pulls/12/files", json!({"per_page": 101}), None),
        ("/repos/o/r/pulls/12/files", json!({"state": "all"}), None),
        ("/repos/o/r/contents/a", json!({"ref": "../main"}), None),
        ("/repos/o/r/contents/a", json!({"ref": "-x"}), None),
        ("/repos/o/r/pulls/12/files", json!(["page", 2]), None),
    ];
    for (path, query, want) in cases {
        let q = (!query.is_null()).then_some(query);
        let got = rest::endpoint(path, q);
        match want {
            Some(e) => assert_eq!(got.as_deref().ok(), Some(*e), "{path} {query}"),
            None => assert_eq!(
                got.err().map(|e| e.class),
                Some(Class::Invalid),
                "{path} {query} should be refused"
            ),
        }
    }
    // A long path is refused before matching.
    let long = format!("/repos/o/r/contents/{}", "a/".repeat(2100));
    assert!(rest::endpoint(&long, None).is_err());
}

#[test]
fn next_page_only_from_same_origin_and_path() {
    let o = "https://api.github.com";
    let e = "repos/o/r/pulls/1/files?page=1";
    let cases: &[(&str, Option<u32>)] = &[
        (
            r#"<https://api.github.com/repos/o/r/pulls/1/files?page=2>; rel="next", <https://api.github.com/repos/o/r/pulls/1/files?page=9>; rel="last""#,
            Some(2),
        ),
        (
            r#"<https://api.github.com/repositories/42/pulls/1/files?per_page=100&page=3>; rel="next""#,
            Some(3),
        ),
        (
            r#"<https://api.github.com/repos/o/r/pulls/1/files?page=1>; rel="prev""#,
            None,
        ),
        (
            r#"<https://evil.example/repos/o/r/pulls/1/files?page=2>; rel="next""#,
            None,
        ),
        (
            r#"<https://api.github.com/repos/o/r/pulls/2/files?page=2>; rel="next""#,
            None,
        ),
        (
            r#"<https://api.github.com/repositories/x/pulls/1/files?page=2>; rel="next""#,
            None,
        ),
        (
            r#"<https://api.github.com/repos/o/r/pulls/1/files?page=1001>; rel="next""#,
            None,
        ),
        (
            r#"<https://api.github.com/repos/o/r/pulls/1/files?page=0>; rel="next""#,
            None,
        ),
    ];
    for (link, want) in cases {
        assert_eq!(rest::next_page(Some(link), o, e), *want, "{link}");
    }
}

// ---- GraphQL ------------------------------------------------------------------------------

#[test]
fn graphql_documents_declare_what_they_check() {
    let mut ids = std::collections::HashSet::new();
    for d in graphql::DOCS {
        assert!(ids.insert(d.id), "{} twice", d.id);
        // The declared variables in the text are exactly the checked ones.
        let header = d.text.split('{').next().unwrap();
        let mut declared: Vec<&str> = header
            .split('$')
            .skip(1)
            .map(|s| s.split(':').next().unwrap().trim())
            .collect();
        let mut checked: Vec<&str> = d.vars.iter().map(|(n, _, _)| *n).collect();
        declared.sort();
        checked.sort();
        assert_eq!(declared, checked, "{}", d.id);
        for (n, _, required) in d.vars {
            let ty = header.split(&format!("${n}:")).nth(1).unwrap();
            let ty = ty.split([',', ')']).next().unwrap().trim();
            assert_eq!(ty.ends_with('!'), *required, "{}: ${n} is {ty}", d.id);
        }
        if graphql::is_mutation(d) {
            // A mutation document calls the field it is named after.
            assert!(d.text.contains(&format!("{}(input:", d.id)), "{}", d.id);
        }
    }
    // The renderer may run queries and the two Viewed mutations; nothing else that writes.
    let renderer_mutations: Vec<&str> = graphql::DOCS
        .iter()
        .filter(|d| !d.native && graphql::is_mutation(d))
        .map(|d| d.id)
        .collect();
    assert_eq!(
        renderer_mutations,
        ["markFileAsViewed", "unmarkFileAsViewed"]
    );
    let script = Script::new(vec![]);
    let g = gh(&script);
    for d in graphql::DOCS.iter().filter(|d| d.native) {
        assert_eq!(
            class(&g.graphql(d.id, None)),
            Some(Class::Refused),
            "{}",
            d.id
        );
    }
    assert_eq!(
        class(&g.graphql("deleteRepository", None)),
        Some(Class::Invalid)
    );
    assert!(script.sent().is_empty(), "nothing reaches GitHub");
}

#[test]
fn graphql_variables_are_checked_against_the_document() {
    let pr = json!({"owner": "o", "repo": "r", "number": 5});
    let cases: &[(&str, Value, Option<Class>)] = &[
        ("viewer", Value::Null, None),
        ("viewer", json!({}), None),
        ("viewer", json!({"x": 1}), Some(Class::Invalid)),
        ("pullRequestSummary", pr.clone(), None),
        (
            "pullRequestSummary",
            json!({"owner": "o", "repo": "r"}),
            Some(Class::Invalid),
        ),
        (
            "pullRequestSummary",
            json!({"owner": "o", "repo": "r", "number": "5"}),
            Some(Class::Invalid),
        ),
        (
            "pullRequestSummary",
            json!({"owner": "o", "repo": "r", "number": 0}),
            Some(Class::Invalid),
        ),
        (
            "pullRequestSummary",
            json!({"owner": "o/x", "repo": "r", "number": 5}),
            Some(Class::Invalid),
        ),
        (
            "pullRequestSummary",
            json!({"owner": "o", "repo": "r", "number": 5, "after": "c"}),
            Some(Class::Invalid),
        ),
        (
            "reviewThreads",
            json!({"owner": "o", "repo": "r", "number": 5, "after": null}),
            None,
        ),
        (
            "reviewThreads",
            json!({"owner": "o", "repo": "r", "number": 5, "after": "Y3Vyc29y"}),
            None,
        ),
        (
            "reviewThreads",
            json!({"owner": "o", "repo": "r", "number": 5, "after": "x".repeat(201)}),
            Some(Class::Invalid),
        ),
        (
            "markFileAsViewed",
            json!({"pullRequestId": "PR_kwDOAbc", "path": "src/a.rs"}),
            None,
        ),
        (
            "markFileAsViewed",
            json!({"pullRequestId": "PR kw", "path": "src/a.rs"}),
            Some(Class::Invalid),
        ),
        (
            "markFileAsViewed",
            json!({"pullRequestId": "PR_kw", "path": "../a.rs"}),
            Some(Class::Invalid),
        ),
        (
            "searchPullRequests",
            json!({"q": "is:pr", "first": 101}),
            Some(Class::Invalid),
        ),
        ("viewer", json!([1]), Some(Class::Invalid)),
    ];
    for (id, vars, want) in cases {
        let doc = graphql::doc(id).unwrap();
        let v = (!vars.is_null()).then_some(vars);
        let got = graphql::body(doc, v).err().map(|e| e.class);
        assert_eq!(got, *want, "{id} {vars}");
    }
    // Over 64 KiB of variables.
    let doc = graphql::doc("searchPullRequests").unwrap();
    let mut big = serde_json::Map::new();
    big.insert("q".into(), json!("x".repeat(500)));
    big.insert("first".into(), json!(10));
    assert!(graphql::body(doc, Some(&Value::Object(big))).is_ok());
    let body_doc = graphql::doc("addPullRequestReviewThreadReply").unwrap();
    let over = json!({"threadId": "T_1", "body": "é".repeat(30_001)});
    assert!(graphql::body(body_doc, Some(&over)).is_err());
}

// ---- answers, errors, rate limits -------------------------------------------------------

#[test]
fn reads_answer_or_fail_with_a_class() {
    let rate = [
        ("x-ratelimit-limit", "5000"),
        ("x-ratelimit-remaining", "4990"),
        ("x-ratelimit-used", "10"),
        ("x-ratelimit-reset", "1800000600"),
    ];
    let link = [(
        "link",
        r#"<https://api.github.com/repos/o/r/pulls/1/files?page=2>; rel="next""#,
    )];
    type Check = fn(&GhResult<Answer>);
    let cases: Vec<(&str, Result<Response, TransportError>, Check)> = vec![
        ("rate info", res(200, &rate, json!({"login": "me"})), |r| {
            let a = r.as_ref().unwrap();
            assert_eq!(a.data["login"], "me");
            let rl = a.rate_limit.as_ref().unwrap();
            assert_eq!(
                (rl.limit, rl.remaining, rl.used, rl.reset_at),
                (5000, 4990, 10, 1_800_000_600_000)
            );
        }),
        ("next page", res(200, &link, json!([])), |r| {
            assert_eq!(r.as_ref().unwrap().next_page, Some(2));
        }),
        (
            "not found is an answer",
            res(404, &[], json!({"message": "Not Found"})),
            |r| {
                assert_eq!(r.as_ref().unwrap().status, 404);
            },
        ),
        (
            "bad sign-in",
            res(401, &[], json!({"message": "Bad credentials"})),
            |r| {
                assert_eq!(class(r), Some(Class::Unauthorized));
            },
        ),
        (
            "unreadable",
            Ok(Response {
                status: 200,
                headers: vec![],
                body: "<html>".into(),
            }),
            |r| {
                assert_eq!(class(r), Some(Class::GitHub));
            },
        ),
        (
            "gh missing",
            Err(TransportError::Missing("not found".into())),
            |r| {
                assert_eq!(class(r), Some(Class::Unavailable));
                assert!(r.as_ref().unwrap_err().message.contains("not installed"));
            },
        ),
        ("gh signed out", Err(TransportError::SignedOut), |r| {
            assert_eq!(class(r), Some(Class::Unauthorized));
        }),
        ("deadline", Err(TransportError::Timeout), |r| {
            assert_eq!(class(r), Some(Class::Network))
        }),
        ("too large", Err(TransportError::TooLarge), |r| {
            assert_eq!(class(r), Some(Class::TooLarge))
        }),
        ("other", Err(TransportError::Failed("boom".into())), |r| {
            assert_eq!(class(r), Some(Class::Network))
        }),
    ];
    for (name, answer, check) in cases {
        let script = Script::new(vec![answer]);
        let g = gh(&script);
        let r = g.rest("/repos/o/r/pulls/1/files", Some(&json!({"page": 1})));
        eprintln!("case: {name}");
        check(&r);
    }

    // GraphQL: errors with no data fail; errors beside data are passed along.
    let cases: Vec<(Value, Option<Class>, bool)> = vec![
        (json!({"data": {"viewer": {"login": "me"}}}), None, false),
        (
            json!({"data": null, "errors": [{"message": "a"}, {"message": "b"}]}),
            Some(Class::GitHub),
            false,
        ),
        (
            json!({"data": {"viewer": null}, "errors": [{"message": "partial"}]}),
            None,
            true,
        ),
    ];
    for (body, want, has_errors) in cases {
        let script = Script::new(vec![ok(body.clone())]);
        let r = gh(&script).graphql("viewer", None);
        assert_eq!(class(&r), want, "{body}");
        if want.is_none() {
            assert_eq!(r.unwrap().errors.is_some(), has_errors, "{body}");
        }
    }
}

#[test]
fn rate_limits_pause_github_for_one_second_to_one_hour() {
    let reset_in = |secs: i64| ((NOW / 1000) + secs).to_string();
    type Case<'a> = (u16, Vec<(&'a str, String)>, Option<i64>);
    let cases: Vec<Case> = vec![
        // Retry-After wins, clamped up to 1 s.
        (429, vec![("retry-after", "0".into())], Some(1_000)),
        (403, vec![("retry-after", "120".into())], Some(120_000)),
        // Remaining 0: until the reset, clamped down to 1 h.
        (
            403,
            vec![
                ("x-ratelimit-remaining", "0".into()),
                ("x-ratelimit-reset", reset_in(300)),
            ],
            Some(300_000),
        ),
        (
            403,
            vec![
                ("x-ratelimit-remaining", "0".into()),
                ("x-ratelimit-reset", reset_in(4 * 3600)),
            ],
            Some(3_600_000),
        ),
        // Remaining 0 without a reset: 60 s.
        (
            429,
            vec![("x-ratelimit-remaining", "0".into())],
            Some(60_000),
        ),
        // A 403 that is not a rate limit is an ordinary answer.
        (403, vec![("x-ratelimit-remaining", "12".into())], None),
    ];
    for (status, headers, pause) in cases {
        let h: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let script = Script::new(vec![res(status, &h, json!({"message": "x"}))]);
        let now = Arc::new(AtomicI64::new(NOW));
        let g = client_at(&script, &now);
        let r = g.rest("/user", None);
        match pause {
            None => assert_eq!(r.unwrap().status, status),
            Some(ms) => {
                assert_eq!(class(&r), Some(Class::RateLimited), "{headers:?}");
                assert_eq!(g.client.paused_until(), Some(NOW + ms), "{headers:?}");
                // While paused, requests fail at once and nothing is sent.
                assert_eq!(class(&g.rest("/user", None)), Some(Class::RateLimited));
                assert_eq!(class(&g.viewer()), Some(Class::RateLimited));
                assert_eq!(script.sent().len(), 1);
                // After the pause, requests go out again.
                now.store(NOW + ms, Ordering::SeqCst);
                script.answers.lock().unwrap().push_back(ok(json!({})));
                assert!(g.rest("/user", None).is_ok());
            }
        }
    }
    // GraphQL reports its limit in a 200.
    let script = Script::new(vec![res(
        200,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &reset_in(30)),
        ],
        json!({"data": null, "errors": [{"type": "RATE_LIMITED", "message": "API rate limit exceeded"}]}),
    )]);
    let g = gh(&script);
    assert_eq!(class(&g.viewer()), Some(Class::RateLimited));
    assert_eq!(g.client.paused_until(), Some(NOW + 30_000));
    g.client.clear_pause();
    assert_eq!(g.client.paused_until(), None);
}

// ---- checked writes -----------------------------------------------------------------------

fn pr_facts(state: &str, head: &str, pending: u64) -> Result<Response, TransportError> {
    ok(json!({"data": {"repository": {"pullRequest": {
        "id": "PR_1", "state": state, "merged": state == "MERGED", "headRefOid": head,
        "pending": {"totalCount": pending}
    }}}}))
}

fn thread(owner: &str, number: u64, resolved: bool) -> Result<Response, TransportError> {
    ok(json!({"data": {"node": {
        "__typename": "PullRequestReviewThread", "id": "T_1", "isResolved": resolved,
        "pullRequest": {"number": number, "repository": {"name": "r", "owner": {"login": owner}}}
    }}}))
}

#[derive(Clone, Copy, Debug)]
enum W {
    Comment,
    ReviewComment,
    Reply,
    Resolve(bool),
    Review(&'static str),
}

fn run(w: W, c: &Client, body: &str, head: &str) -> GhResult<Answer> {
    match w {
        W::Comment => writes::comment(
            c,
            &serde_json::from_value(json!({"owner": "o", "repo": "r", "number": 7, "body": body})).unwrap(),
        ),
        W::ReviewComment => writes::review_comment(
            c,
            &serde_json::from_value(json!({
                "owner": "o", "repo": "r", "number": 7, "head": head, "path": "src/a.rs",
                "line": 12, "side": "RIGHT", "startLine": 10, "body": body
            }))
            .unwrap(),
        ),
        W::Reply => writes::review_reply(
            c,
            &serde_json::from_value(json!({"owner": "o", "repo": "r", "number": 7, "threadId": "T_1", "body": body})).unwrap(),
        ),
        W::Resolve(on) => writes::review_resolve(
            c,
            &serde_json::from_value(json!({"threadId": "T_1", "resolve": on})).unwrap(),
        ),
        W::Review(event) => writes::review(
            c,
            &serde_json::from_value(json!({"owner": "o", "repo": "r", "number": 7, "head": head, "event": event, "body": body})).unwrap(),
        ),
    }
}

#[test]
fn checked_writes_read_first_and_send_once() {
    use Class::*;
    let created = || res(201, &[], json!({"id": 99}));
    // (write, body text, head, script, outcome, requests sent, last request is the write)
    type Case = (
        W,
        &'static str,
        &'static str,
        Vec<Result<Response, TransportError>>,
        Option<Class>,
        usize,
    );
    let cases: Vec<Case> = vec![
        // Comments: read the PR, post once. Closed PRs may be commented on.
        (
            W::Comment,
            "hi",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 0), created()],
            None,
            2,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![pr_facts("MERGED", HEAD, 0), created()],
            None,
            2,
        ),
        (W::Comment, "  ", HEAD, vec![], Some(Invalid), 0),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![ok(
                json!({"data": {"repository": {"pullRequest": null}}, "errors": [{"type": "NOT_FOUND", "message": "nope"}]}),
            )],
            Some(Refused),
            1,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![res(502, &[], Value::Null)],
            Some(Refused),
            1,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![Err(TransportError::Timeout)],
            Some(Network),
            1,
        ),
        // The write's own outcome: a 4xx is refused, anything else unconfirmed.
        (
            W::Comment,
            "hi",
            HEAD,
            vec![
                pr_facts("OPEN", HEAD, 0),
                res(
                    422,
                    &[],
                    json!({"message": "Validation Failed", "errors": [{"message": "body is too long"}]}),
                ),
            ],
            Some(Refused),
            2,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 0), res(502, &[], Value::Null)],
            Some(Uncertain),
            2,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 0), Err(TransportError::Timeout)],
            Some(Uncertain),
            2,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![
                pr_facts("OPEN", HEAD, 0),
                Err(TransportError::Failed("reset".into())),
            ],
            Some(Uncertain),
            2,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 0), Err(TransportError::TooLarge)],
            Some(Uncertain),
            2,
        ),
        (
            W::Comment,
            "hi",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 0), Err(TransportError::SignedOut)],
            Some(Unauthorized),
            2,
        ),
        // Single review comment: pinned to the head the page showed; no pending review.
        (
            W::ReviewComment,
            "nit",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 0), created()],
            None,
            2,
        ),
        (
            W::ReviewComment,
            "nit",
            HEAD,
            vec![pr_facts("OPEN", OTHER, 0)],
            Some(HeadMoved),
            1,
        ),
        (
            W::ReviewComment,
            "nit",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 1)],
            Some(ReviewPending),
            1,
        ),
        (W::ReviewComment, "nit", "abc", vec![], Some(Invalid), 0),
        // Replies: the thread must be on this PR.
        (
            W::Reply,
            "ok",
            HEAD,
            vec![
                thread("o", 7, false),
                pr_facts("OPEN", HEAD, 0),
                ok(
                    json!({"data": {"addPullRequestReviewThreadReply": {"comment": {"id": "C_1"}}}}),
                ),
            ],
            None,
            3,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![
                thread("O", 7, false),
                pr_facts("OPEN", HEAD, 0),
                ok(
                    json!({"data": {"addPullRequestReviewThreadReply": {"comment": {"id": "C_1"}}}}),
                ),
            ],
            None,
            3,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![thread("o", 8, false)],
            Some(Refused),
            1,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![thread("x", 7, false)],
            Some(Refused),
            1,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![ok(json!({"data": {"node": {"__typename": "Issue"}}}))],
            Some(Refused),
            1,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![thread("o", 7, false), pr_facts("OPEN", HEAD, 1)],
            Some(ReviewPending),
            2,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![
                thread("o", 7, false),
                pr_facts("OPEN", HEAD, 0),
                ok(
                    json!({"data": {"addPullRequestReviewThreadReply": null}, "errors": [{"message": "locked"}]}),
                ),
            ],
            Some(Refused),
            3,
        ),
        (
            W::Reply,
            "ok",
            HEAD,
            vec![
                thread("o", 7, false),
                pr_facts("OPEN", HEAD, 0),
                ok(json!({})),
            ],
            Some(Uncertain),
            3,
        ),
        // Resolve: no write when it is already in that state.
        (
            W::Resolve(true),
            "",
            HEAD,
            vec![
                thread("o", 7, false),
                ok(
                    json!({"data": {"resolveReviewThread": {"thread": {"id": "T_1", "isResolved": true}}}}),
                ),
            ],
            None,
            2,
        ),
        (
            W::Resolve(true),
            "",
            HEAD,
            vec![thread("o", 7, true)],
            None,
            1,
        ),
        (
            W::Resolve(false),
            "",
            HEAD,
            vec![
                thread("o", 7, true),
                ok(
                    json!({"data": {"unresolveReviewThread": {"thread": {"id": "T_1", "isResolved": false}}}}),
                ),
            ],
            None,
            2,
        ),
        // Reviews: a new review at the head, on an open PR.
        (
            W::Review("APPROVE"),
            "",
            HEAD,
            vec![
                pr_facts("OPEN", HEAD, 0),
                ok(json!({"id": 5, "state": "APPROVED"})),
            ],
            None,
            2,
        ),
        (
            W::Review("REQUEST_CHANGES"),
            "",
            HEAD,
            vec![],
            Some(Invalid),
            0,
        ),
        (W::Review("DISMISS"), "x", HEAD, vec![], Some(Invalid), 0),
        (
            W::Review("COMMENT"),
            "x",
            HEAD,
            vec![pr_facts("CLOSED", HEAD, 0)],
            Some(Closed),
            1,
        ),
        (
            W::Review("COMMENT"),
            "x",
            HEAD,
            vec![pr_facts("OPEN", OTHER, 0)],
            Some(HeadMoved),
            1,
        ),
        (
            W::Review("COMMENT"),
            "x",
            HEAD,
            vec![pr_facts("OPEN", HEAD, 1)],
            Some(ReviewPending),
            1,
        ),
    ];
    for (n, (w, body, head, script, want, sent)) in cases.into_iter().enumerate() {
        let script = Script::new(script);
        let g = gh(&script);
        let r = run(w, &g.client, body, head);
        assert_eq!(class(&r), want, "case {n} {w:?}: {r:?}");
        let reqs = script.sent();
        assert_eq!(reqs.len(), sent, "case {n} {w:?}");
        assert!(
            script.answers.lock().unwrap().is_empty(),
            "case {n}: script not used up"
        );
        // Nothing is ever retried: each request is distinct.
        if want.is_none() && reqs.len() >= 2 {
            let last = reqs.last().unwrap();
            assert!(last.body.is_some(), "case {n}");
        }
    }

    // What the writes send is built from typed fields and the read, never extra input.
    let script = Script::new(vec![pr_facts("OPEN", HEAD, 0), res(201, &[], json!({}))]);
    run(W::ReviewComment, &gh(&script).client, "nit", HEAD).unwrap();
    let w = script.sent().pop().unwrap();
    assert_eq!(w.endpoint, "repos/o/r/pulls/7/comments");
    assert_eq!(
        w.body.unwrap(),
        json!({"body": "nit", "commit_id": HEAD, "path": "src/a.rs", "line": 12, "side": "RIGHT", "start_line": 10, "start_side": "RIGHT"})
    );
    // Typed fields only: a page-sent commit id or event is refused before anything is read.
    for extra in [json!({"commitId": HEAD}), json!({"pullRequestId": "PR_1"})] {
        let mut a = json!({"owner": "o", "repo": "r", "number": 7, "body": "x"});
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<writes::CommentArgs>(a).is_err());
    }
    // A long body is refused before the read.
    let script = Script::new(vec![]);
    assert_eq!(
        class(&run(
            W::Comment,
            &gh(&script).client,
            &"x".repeat(60_001),
            HEAD
        )),
        Some(TooLarge)
    );
    // A write waits on the rate pause like a read.
    let script = Script::new(vec![res(429, &[("retry-after", "5")], json!({}))]);
    let g = gh(&script);
    assert_eq!(
        class(&run(W::Comment, &g.client, "hi", HEAD)),
        Some(RateLimited)
    );
    assert_eq!(
        class(&run(W::Comment, &g.client, "hi", HEAD)),
        Some(RateLimited)
    );
    assert_eq!(script.sent().len(), 1);
}

// ---- search, viewer ----------------------------------------------------------------------

#[test]
fn search_and_viewer_use_their_documents() {
    let script = Script::new(vec![
        ok(json!({"data": {"search": {"issueCount": 0, "nodes": []}}})),
        ok(
            json!({"data": {"viewer": {"login": "me", "name": "Me", "avatarUrl": "https://x/a.png"}}}),
        ),
    ]);
    let g = gh(&script);
    g.search_prs(" fix\nlogin ").unwrap();
    assert_eq!(g.viewer().unwrap().data["login"], "me");
    let sent = script.sent();
    assert_eq!(
        sent[0].body.as_ref().unwrap()["variables"],
        json!({"q": "is:pr involves:@me fix login", "first": 10})
    );
    assert_eq!(sent[1].endpoint, "graphql");
    assert_eq!(class(&g.search_prs(&"x".repeat(257))), Some(Class::Invalid));
}

// ---- the switch and the ops ----------------------------------------------------------------

#[test]
fn the_switch_is_compare_and_swap_and_gates_every_network_op() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        Database::open(
            &dir.path().join("t.sqlite3"),
            &helm_core::db::MigrationRegistry::new(),
        )
        .unwrap(),
    );
    let script = Script::new(vec![]);
    let mut services = Services::new();
    services.insert(db.clone());
    services.insert(Arc::new(gh(&script)));
    let sink = Arc::new(RecordingSink::default());
    let ctx = OpCtx {
        services: Arc::new(services),
        events: sink.clone(),
    };

    // Default off, revision 0; any argument to get is refused.
    assert_eq!(
        settings_get(&ctx, Value::Null).unwrap(),
        json!({"enabled": false, "revision": 0})
    );
    assert!(settings_get(&ctx, json!({"x": 1})).is_err());

    // Off: every network op is refused before anything is sent.
    let mut reg = OpRegistry::new();
    register_ops(&mut reg);
    let api = helm_ipc::Dispatcher::new(reg, ctx.clone(), false);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pr = json!({"owner": "o", "repo": "r", "number": 1});
    let with = |extra: Value| {
        let mut v = pr.clone();
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    let calls: Vec<(&str, Value)> = vec![
        ("github_viewer", json!({})),
        ("github_rest", json!({"path": "/user"})),
        ("github_graphql", json!({"id": "viewer"})),
        ("github_search_prs", json!({"query": "x"})),
        ("github_comment", with(json!({"body": "b"}))),
        (
            "github_review_comment",
            with(json!({"head": HEAD, "path": "a", "line": 1, "side": "RIGHT", "body": "b"})),
        ),
        (
            "github_review_reply",
            with(json!({"threadId": "T_1", "body": "b"})),
        ),
        (
            "github_review_resolve",
            json!({"threadId": "T_1", "resolve": true}),
        ),
        (
            "github_review",
            with(json!({"head": HEAD, "event": "APPROVE", "body": ""})),
        ),
    ];
    for (op, args) in &calls {
        let e = rt.block_on(api.call_host(op, args.clone())).unwrap_err();
        assert!(e.message.starts_with("[Disabled] "), "{op}: {}", e.message);
    }
    // Unknown fields are refused (a page-sent method, host or node id).
    for (op, args) in [
        ("github_rest", json!({"path": "/user", "method": "POST"})),
        (
            "github_graphql",
            json!({"id": "viewer", "query": "mutation { x }"}),
        ),
        (
            "github_review",
            with(json!({"head": HEAD, "event": "APPROVE", "body": "", "pullRequestId": "PR_1"})),
        ),
    ] {
        let e = rt.block_on(api.call_host(op, args)).unwrap_err();
        assert!(e.message.starts_with("[Invalid] "), "{op}: {}", e.message);
    }
    assert!(script.sent().is_empty());

    // Save is compare-and-swap.
    let on = settings_save(&ctx, json!({"enabled": true, "expectedRevision": 0})).unwrap();
    assert_eq!(on, json!({"enabled": true, "revision": 1}));
    let stale = settings_save(&ctx, json!({"enabled": false, "expectedRevision": 0})).unwrap_err();
    assert!(stale.message.starts_with("[Refused] "), "{}", stale.message);
    assert!(settings_save(
        &ctx,
        json!({"enabled": true, "expectedRevision": 1, "hosts": []})
    )
    .is_err());
    assert_eq!(sink.kinds(), ["github_settings"]);
    assert!(gate(&ctx).is_ok());
    for (op, args) in &calls[..1] {
        script
            .answers
            .lock()
            .unwrap()
            .push_back(ok(json!({"data": {"viewer": {"login": "me"}}})));
        let a = rt.block_on(api.call_host(op, args.clone())).unwrap();
        assert_eq!(a["data"]["login"], "me");
        assert_eq!(a["status"], 200);
    }

    // An unreadable stored value reads as off.
    db.write(|tx| prefs::set(tx, PR_VIEW, &json!({"enabled": "yes"})))
        .unwrap();
    assert_eq!(settings(&db), Settings::default());
    assert!(gate(&ctx).is_err());
}

// ---- gh itself ----------------------------------------------------------------------------

#[test]
fn gh_output_is_read_with_its_headers() {
    for sep in ["\r\n", "\n"] {
        let out = format!(
            "HTTP/2.0 404 Not Found{sep}X-Ratelimit-Remaining: 7{sep}Link: <a>; rel=\"next\"{sep}{sep}{{\"message\":\"Not Found\"}}"
        );
        let r = parse_included(&out).unwrap();
        assert_eq!(r.status, 404);
        assert_eq!(r.header("x-ratelimit-remaining"), Some("7"));
        assert_eq!(r.header("link"), Some("<a>; rel=\"next\""));
        assert_eq!(r.body, "{\"message\":\"Not Found\"}");
    }
    assert_eq!(
        parse_included("HTTP/1.1 204 No Content\r\n\r\n")
            .unwrap()
            .body,
        ""
    );
    assert!(parse_included("").is_none());
    assert!(parse_included("error: something").is_none());

    // gh printed no answer: signed out or a plain failure, scrubbed and bounded.
    use transport::classify_gh_failure as f;
    assert_eq!(f(Some(4), ""), TransportError::SignedOut);
    assert_eq!(
        f(
            Some(1),
            "To get started with GitHub CLI, please run:  gh auth login"
        ),
        TransportError::SignedOut
    );
    assert_eq!(
        f(
            Some(1),
            "\nerror connecting to api.github.com ghp_abcdefghijklmnopqrstuvwxyz0123\n"
        ),
        TransportError::Failed("error connecting to api.github.com [token]".into())
    );
}

#[test]
fn tokens_never_reach_text() {
    let cases = [
        (
            "token ghp_ABCDEFGHIJKLMNOPQRSTUVWX01 here",
            "token [token] here",
        ),
        ("github_pat_11ABCDEFG0123456789_abcdefghijklmnop", "[token]"),
        ("gho_short is fine", "gho_short is fine"),
        ("no secrets", "no secrets"),
    ];
    for (input, want) in cases {
        assert_eq!(scrub(input), want);
    }
    // GitHub echoing a token in an error message is scrubbed too.
    let script = Script::new(vec![
        pr_facts("OPEN", HEAD, 0),
        res(
            422,
            &[],
            json!({"message": "bad ghs_0123456789abcdefghijKLMN"}),
        ),
    ]);
    let e = run(W::Comment, &gh(&script).client, "hi", HEAD).unwrap_err();
    assert_eq!(
        e.to_string(),
        "[Refused] GitHub refused it (422): bad [token]"
    );
    assert_eq!(clean("a\nb\tc", 100), "a b c");
    assert_eq!(clean("abcdef", 4), "abc…");
}

#[test]
fn gh_is_only_ever_asked_for_the_api() {
    let reqs = [
        Request {
            method: Method::Get,
            endpoint: "repos/o/r/pulls/1".into(),
            body: None,
        },
        Request {
            method: Method::Post,
            endpoint: "graphql".into(),
            body: Some(json!({"query": "q"})),
        },
    ];
    for r in &reqs {
        let argv = gh_argv(r);
        assert_eq!(&argv[..4], ["api", "--hostname", "github.com", "-i"]);
        assert!(
            argv.iter().all(|a| a != "auth" && !a.contains("token")),
            "{argv:?}"
        );
        // A body never rides on the command line.
        assert_eq!(
            argv.ends_with(&["--input".into(), "-".into()]),
            r.body.is_some()
        );
    }
    assert!(!transport::GH_PROGRAM.ends_with(".cmd") && !transport::GH_PROGRAM.ends_with(".bat"));

    // Source scan: no `auth` argument to gh, no token printing, nothing that reads gh's
    // configuration, and gh is spawned in one place.
    let auth = format!("\"{}\"", "auth");
    let needles = [
        auth.as_str(),
        concat!("--show", "-token"),
        concat!("hosts", ".yml"),
        concat!("GH_CONFIG", "_DIR"),
        concat!("config", "_dir"),
    ];
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut spawns = 0;
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() == "tests.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for n in needles {
            assert!(!text.contains(n), "{} contains {n}", path.display());
        }
        spawns += text.matches(concat!("Command", "::new(")).count();
        if text.contains(concat!("Command", "::new(")) {
            assert!(
                text.contains(concat!("Command", "::new(GH_PROGRAM)")),
                "{}",
                path.display()
            );
        }
    }
    assert_eq!(spawns, 1);
}

#[test]
fn the_test_transport_is_loopback_only() {
    let cases = [
        ("http://127.0.0.1:5123", true),
        ("http://localhost:5123/", true),
        ("http://[::1]:5123", true),
        ("https://127.0.0.1:5123", false),
        ("http://127.0.0.1", false),
        ("http://127.0.0.1:5123/api", false),
        ("http://example.com:80", false),
        ("http://127.0.0.2:80", false),
        ("http://user@127.0.0.1:80", false),
    ];
    for (url, ok) in cases {
        assert_eq!(transport::Loopback::new(url).is_some(), ok, "{url}");
    }
}

/// Live, reads only: `HELM_LIVE_GITHUB=1 cargo test -p helm-github live_reads -- --nocapture`
/// runs the real `gh` under your own sign-in.
#[test]
fn live_reads_through_gh() {
    if std::env::var("HELM_LIVE_GITHUB").as_deref() != Ok("1") {
        return;
    }
    let g = GitHub::new(Arc::new(transport::GhCli));
    let v = g.viewer().unwrap();
    assert!(v.data["login"].is_string(), "{v:?}");
    assert!(v.rate_limit.is_some());
    let a = g
        .rest(
            "/repos/cli/cli/pulls/5/files",
            Some(&json!({"per_page": 1})),
        )
        .unwrap();
    assert_eq!(a.status, 200);
    assert_eq!(a.next_page, Some(2), "{:?}", a.rate_limit);
    let s = g
        .graphql(
            "pullRequestSummary",
            Some(&json!({"owner": "cli", "repo": "cli", "number": 1})),
        )
        .unwrap();
    assert!(
        s.data["repository"]["pullRequest"]["title"].is_string(),
        "{s:?}"
    );
    for id in [
        "pullRequestTimeline",
        "reviewThreads",
        "statusCheckRollup",
        "pullRequestCommits",
        "pullRequestFileViewedStates",
    ] {
        let a = g
            .graphql(
                id,
                Some(&json!({"owner": "cli", "repo": "cli", "number": 1})),
            )
            .unwrap();
        assert!(a.errors.is_none(), "{id}: {:?}", a.errors);
    }
    let r = g.search_prs("").unwrap();
    assert!(r.data["search"]["issueCount"].is_number());
    let missing = g.rest("/repos/cli/cli/pulls/999999999", None).unwrap();
    assert_eq!(missing.status, 404);
    // The read a checked write makes, against a real pull request.
    assert!(g
        .client
        .read(&Request {
            method: Method::Post,
            endpoint: "graphql".into(),
            body: Some(
                graphql::body(
                    graphql::doc("pullRequestForWrite").unwrap(),
                    Some(&json!({"owner": "cli", "repo": "cli", "number": 1}))
                )
                .unwrap()
            ),
        })
        .unwrap()
        .errors
        .is_none());
}
