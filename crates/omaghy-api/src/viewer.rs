//! Who the token belongs to.
//!
//! Not cosmetic: every cache row is keyed by viewer, because
//! `i_am_requested`, `my_review` and `unread` all answer *"does this need
//! me"* (`spec/20-store.md` §3.1). The cache therefore cannot be opened until
//! this has been answered, which makes it the first request omaghy makes.

use crate::{client::GitHubClient, graphql::GraphQlRequest};
use omaghy_model::StoreError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ViewerQuery {
    viewer: Login,
}

#[derive(Debug, Deserialize)]
struct Login {
    login: String,
}

/// The login the token authenticates as.
///
/// One point of the GraphQL budget, once per start.
pub async fn viewer_login(client: &GitHubClient) -> Result<String, StoreError> {
    let request = GraphQlRequest::query("query { viewer { login } }");
    let data: ViewerQuery = client.graphql(&request).await?;
    Ok(data.viewer.login)
}
