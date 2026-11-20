//! The little of GitHub's API that connecting a folder needs: signing in with
//! the device flow, and finding or creating the repository to sync with.
//!
//! The device flow needs no client secret, so nothing here is confidential:
//! the client id identifies Suzuri's OAuth app (owned by the datamonet
//! organization) and is meant to ship in the binary.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, Method, Request};
use serde::{Deserialize, de::DeserializeOwned};

const CLIENT_ID: &str = "Ov23liiwg7W1D5RVox5d";
/// `repo` is the narrowest scope a classic OAuth app can ask for that allows
/// creating and pushing to a private repository.
const SCOPE: &str = "repo";

pub const GITHUB: Hosts<'static> = Hosts {
    web: "https://github.com",
    api: "https://api.github.com",
};

#[derive(Clone, Copy)]
pub struct Hosts<'a> {
    pub web: &'a str,
    pub api: &'a str,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, PartialEq)]
pub enum Poll {
    Pending,
    /// GitHub asks for a longer wait between polls from now on.
    SlowDown,
    Token(String),
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct User {
    pub login: String,
    pub id: u64,
    pub name: Option<String>,
}

impl User {
    /// The address GitHub attributes commits to without exposing a real one.
    pub fn noreply_email(&self) -> String {
        format!("{}+{}@users.noreply.github.com", self.id, self.login)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Repo {
    pub full_name: String,
    pub clone_url: String,
    /// Kilobytes of content; zero for a repository nothing was pushed to.
    #[serde(default)]
    pub size: u64,
}

pub async fn request_device_code(
    http: &Arc<dyn HttpClient>,
    hosts: Hosts<'_>,
) -> Result<DeviceCode> {
    let body = format!("client_id={CLIENT_ID}&scope={SCOPE}");
    let (status, text) = send(
        http,
        Method::POST,
        &format!("{}/login/device/code", hosts.web),
        None,
        Some(("application/x-www-form-urlencoded", body)),
    )
    .await?;
    parse(status, &text).context("starting the GitHub sign-in")
}

pub async fn poll_for_token(
    http: &Arc<dyn HttpClient>,
    hosts: Hosts<'_>,
    device_code: &str,
) -> Result<Poll> {
    #[derive(Deserialize)]
    struct Response {
        access_token: Option<String>,
        error: Option<String>,
        error_description: Option<String>,
    }
    let body = format!(
        "client_id={CLIENT_ID}&device_code={device_code}&grant_type=urn:ietf:params:oauth:grant-type:device_code"
    );
    let (status, text) = send(
        http,
        Method::POST,
        &format!("{}/login/oauth/access_token", hosts.web),
        None,
        Some(("application/x-www-form-urlencoded", body)),
    )
    .await?;
    let response: Response = parse(status, &text)?;
    if let Some(token) = response.access_token {
        return Ok(Poll::Token(token));
    }
    match response.error.as_deref() {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token") => bail!("the sign-in code expired before it was approved"),
        Some("access_denied") => bail!("the sign-in was cancelled on GitHub"),
        other => bail!(
            "GitHub refused the sign-in: {}",
            response
                .error_description
                .as_deref()
                .or(other)
                .unwrap_or("unknown error")
        ),
    }
}

pub async fn user(http: &Arc<dyn HttpClient>, hosts: Hosts<'_>, token: &str) -> Result<User> {
    let (status, text) = send(
        http,
        Method::GET,
        &format!("{}/user", hosts.api),
        Some(token),
        None,
    )
    .await?;
    parse(status, &text).context("reading your GitHub account")
}

/// The signed-in user's own repositories, most recently changed first.
pub async fn list_repos(
    http: &Arc<dyn HttpClient>,
    hosts: Hosts<'_>,
    token: &str,
) -> Result<Vec<Repo>> {
    let (status, text) = send(
        http,
        Method::GET,
        &format!(
            "{}/user/repos?affiliation=owner&sort=pushed&per_page=100",
            hosts.api
        ),
        Some(token),
        None,
    )
    .await?;
    parse(status, &text).context("listing your GitHub repositories")
}

pub async fn create_private_repo(
    http: &Arc<dyn HttpClient>,
    hosts: Hosts<'_>,
    token: &str,
    name: &str,
) -> Result<Repo> {
    let body = serde_json::json!({ "name": name, "private": true }).to_string();
    let (status, text) = send(
        http,
        Method::POST,
        &format!("{}/user/repos", hosts.api),
        Some(token),
        Some(("application/json", body)),
    )
    .await?;
    if status == 422 && text.contains("already exists") {
        bail!("you already have a repository named “{name}”");
    }
    parse(status, &text).with_context(|| format!("creating the repository “{name}”"))
}

/// A repository name GitHub accepts, made from a folder name: letters,
/// digits, `-`, `_` and `.`, with everything else collapsed into dashes.
pub fn repo_name_for(folder: &str) -> String {
    let mut name = String::new();
    for character in folder.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            name.push(character.to_ascii_lowercase());
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches(|character| matches!(character, '-' | '.'));
    if name.is_empty() {
        "notes".to_string()
    } else {
        name.to_string()
    }
}

async fn send(
    http: &Arc<dyn HttpClient>,
    method: Method,
    url: &str,
    token: Option<&str>,
    body: Option<(&str, String)>,
) -> Result<(u16, String)> {
    let mut request = Request::builder()
        .method(method)
        .uri(url)
        .header("Accept", "application/json")
        .header("User-Agent", "Suzuri");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some((content_type, body)) => {
            request = request.header("Content-Type", content_type);
            AsyncBody::from(body)
        }
        None => AsyncBody::default(),
    };
    let mut response = http
        .send(request.body(body)?)
        .await
        .context("could not reach GitHub")?;
    let mut text = String::new();
    response.body_mut().read_to_string(&mut text).await?;
    Ok((response.status().as_u16(), text))
}

fn parse<T: DeserializeOwned>(status: u16, text: &str) -> Result<T> {
    #[derive(Deserialize)]
    struct ApiError {
        message: String,
    }
    if !(200..300).contains(&status) {
        let message = serde_json::from_str::<ApiError>(text)
            .map(|error| error.message)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(anyhow!("GitHub answered: {message}"));
    }
    serde_json::from_str(text).context("GitHub sent a reply Suzuri could not read")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::{FakeHttpClient, Response};

    const TEST: Hosts<'static> = Hosts {
        web: "https://github.test",
        api: "https://api.github.test",
    };

    fn fake(
        reply: impl Fn(&str, &str, Option<String>) -> (u16, &'static str) + Send + Sync + 'static,
    ) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |request| {
            let authorization = request
                .headers()
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let (status, body) = reply(
                request.method().as_str(),
                &request.uri().to_string(),
                authorization,
            );
            async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body.to_string()))?)
            }
        })
    }

    #[gpui::test]
    async fn device_flow_goes_from_pending_to_a_token() {
        let http = fake(|method, url, _| {
            assert_eq!(method, "POST");
            assert_eq!(url, "https://github.test/login/device/code");
            (
                200,
                r#"{"device_code":"dc","user_code":"AB12-CD34","verification_uri":"https://github.test/login/device","expires_in":899,"interval":5}"#,
            )
        });
        let code = request_device_code(&http, TEST).await.unwrap();
        assert_eq!(code.user_code, "AB12-CD34");
        assert_eq!(code.interval, 5);

        for (reply, expected) in [
            (r#"{"error":"authorization_pending"}"#, Poll::Pending),
            (r#"{"error":"slow_down"}"#, Poll::SlowDown),
            (
                r#"{"access_token":"gho_x","token_type":"bearer","scope":"repo"}"#,
                Poll::Token("gho_x".into()),
            ),
        ] {
            let http = fake(move |_, _, _| (200, reply));
            assert_eq!(poll_for_token(&http, TEST, "dc").await.unwrap(), expected);
        }

        let http = fake(|_, _, _| (200, r#"{"error":"access_denied"}"#));
        let error = poll_for_token(&http, TEST, "dc").await.unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    #[gpui::test]
    async fn api_calls_send_the_token_and_report_github_errors() {
        let http = fake(|_, url, authorization| {
            assert_eq!(authorization.as_deref(), Some("Bearer gho_x"));
            match url {
                "https://api.github.test/user" => (200, r#"{"login":"ada","id":7,"name":null}"#),
                _ => (
                    200,
                    r#"[{"full_name":"ada/notes","clone_url":"https://github.test/ada/notes.git","size":0}]"#,
                ),
            }
        });
        let user = user(&http, TEST, "gho_x").await.unwrap();
        assert_eq!(user.noreply_email(), "7+ada@users.noreply.github.com");
        let repos = list_repos(&http, TEST, "gho_x").await.unwrap();
        assert_eq!(repos[0].full_name, "ada/notes");

        let http = fake(|_, _, _| {
            (
                422,
                r#"{"message":"Repository creation failed.","errors":[{"message":"name already exists on this account"}]}"#,
            )
        });
        let error = create_private_repo(&http, TEST, "gho_x", "notes")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already have"), "{error}");

        let http = fake(|_, _, _| (401, r#"{"message":"Bad credentials"}"#));
        let error = super::user(&http, TEST, "gho_x").await.unwrap_err();
        assert!(
            format!("{error:#}").contains("Bad credentials"),
            "{error:#}"
        );
    }

    #[test]
    fn repo_names_are_made_from_folder_names() {
        assert_eq!(repo_name_for("My Notes"), "my-notes");
        assert_eq!(repo_name_for("  Thesis (draft 2)  "), "thesis-draft-2");
        assert_eq!(repo_name_for("研究"), "notes");
        assert_eq!(repo_name_for("paper_v1.2"), "paper_v1.2");
    }
}
