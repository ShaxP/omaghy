//! GitHub search — counts first, rows when a surface can render them.
//!
//! A dashboard section is a saved search (`spec/40-config.md` §2), and until
//! M2 the only thing rendered for one is its **total**. So this module answers
//! "how many match" for a batch of queries in a single request, the same shape
//! [`crate::notifications`] uses for enrichment: one alias per question, and
//! per-alias errors kept rather than failing the batch.
//!
//! Measured against the live API while writing this: four aliased searches
//! cost **1 point of 5000**, the same as one.
//!
//! # Search never says no
//!
//! This is the thing to know before trusting a number from here, and it was
//! verified against the live API rather than assumed:
//!
//! | Query | Answer |
//! |---|---|
//! | `repo:no-such-org-xyz/nope is:open` | `0`, no error |
//! | `is:open archived:maybe` (invalid value) | `139852413` — the qualifier is **ignored** |
//! | `is:open "unclosed` (unbalanced quote) | `75133` |
//! | `` (empty) | `725870294` |
//!
//! **There is no malformed-query error to catch.** A typo does not fail; it
//! silently widens or narrows the search and returns a confident number. So
//! the per-alias error handling below is defensive and will almost never fire,
//! and a count is only ever as trustworthy as the query that produced it —
//! which is a config-validation problem, not one this module can solve
//! (`spec/40-config.md` §3).

use crate::{client::GitHubClient, graphql::GraphQlRequest};
use omaghy_model::StoreError;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// How many searches go in one request.
///
/// Each is one aliased field and the batch costs a single point regardless, so
/// the cap is about request size and error blast radius rather than budget.
/// The dashboard ships four sections and a user may add more.
pub const SEARCH_BATCH: usize = 20;

/// What one aliased `search` field returns when we ask only for the total.
#[derive(Debug, Clone, Deserialize)]
struct CountField {
    #[serde(rename = "issueCount")]
    issue_count: u32,
}

/// Search operations on a [`GitHubClient`].
#[derive(Debug, Clone, Copy)]
pub struct Search<'a> {
    client: &'a GitHubClient,
}

impl GitHubClient {
    /// The search surface of the API.
    pub fn search(&self) -> Search<'_> {
        Search { client: self }
    }
}

impl<'a> Search<'a> {
    pub fn new(client: &'a GitHubClient) -> Self {
        Self { client }
    }

    /// Total matches for each query, in the order given.
    ///
    /// One entry per query. `Err` is GitHub's message for *that* alias — a
    /// section that could not be counted, beside sections that could. The
    /// whole call fails only when the response carries no data at all: a rate
    /// limit, an expired token, a query this crate built wrong.
    pub async fn counts(&self, queries: &[String]) -> Result<Vec<Result<u32, String>>, StoreError> {
        let mut out = Vec::with_capacity(queries.len());
        for chunk in queries.chunks(SEARCH_BATCH) {
            out.extend(self.counts_batch(chunk).await?);
        }
        Ok(out)
    }

    async fn counts_batch(&self, chunk: &[String]) -> Result<Vec<Result<u32, String>>, StoreError> {
        let answer = self
            .client
            .graphql_partial::<HashMap<String, Option<CountField>>>(&build_query(chunk))
            .await?;
        let data = answer.data.clone().unwrap_or_default();

        Ok((0..chunk.len())
            .map(|slot| {
                let alias = alias_name(slot);
                if let Some(message) = answer.message_for(&alias) {
                    return Err(message);
                }
                match data.get(&alias) {
                    Some(Some(field)) => Ok(field.issue_count),
                    // Null with no error of its own. Never observed live, but
                    // the alternative is unwrapping a `None` GitHub is free to
                    // send, so it is reported rather than counted as zero —
                    // zero is a claim.
                    _ => Err(format!("search `{alias}` returned no count")),
                }
            })
            .collect())
    }
}

fn alias_name(slot: usize) -> String {
    format!("s{slot}")
}

/// Build one query for a batch of searches.
///
/// The search strings travel as **variables**, never interpolated. They come
/// from `config.toml`, which is a file this process does not write, and a
/// query assembled by `format!` is an injection waiting to happen. Only the
/// aliases are generated.
///
/// `type: ISSUE` covers issues and pull requests both — the four default
/// sections need exactly that pair. No pagination argument is passed: asking
/// for `issueCount` alone is what makes this cheap, and `first: 0` is not a
/// legal connection argument.
fn build_query(queries: &[String]) -> GraphQlRequest {
    let mut declarations = Vec::with_capacity(queries.len());
    let mut selections = Vec::with_capacity(queries.len());
    let mut variables = Map::new();

    for (slot, query) in queries.iter().enumerate() {
        let q = format!("q{slot}");
        declarations.push(format!("${q}: String!"));
        selections.push(format!(
            "  {}: search(query: ${q}, type: ISSUE) {{ issueCount }}",
            alias_name(slot)
        ));
        variables.insert(q, Value::String(query.clone()));
    }

    let query = format!(
        "query DashboardCounts({}) {{\n{}\n}}",
        declarations.join(", "),
        selections.join("\n"),
    );
    GraphQlRequest::query(query).variables(Value::Object(variables))
}
