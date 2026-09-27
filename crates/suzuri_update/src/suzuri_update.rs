//! Where Suzuri's releases live, for the updater and the remote server.
//!
//! Suzuri ships on the `dev` release channel, whose release endpoints serve
//! Zed's builds rather than this fork's. Zed's own updater (`auto_update`)
//! does the downloading, installing and restarting; this crate is what it
//! asks, on that channel, for the running version and for the newest
//! published build, both of which come from this repository's GitHub
//! releases rather than Zed's release index.
//!
//! The version being compared is [`SUZURI_VERSION`], not the crate version —
//! see that constant for why.
//!
//! The same constant also names the release a running build fetches its
//! remote server from; see [`remote_server_download_url`].

use anyhow::{Context as _, Result};
use http_client::{
    HttpClient,
    github::{GithubRelease, GithubReleaseAsset, latest_github_release},
};
use semver::Version;
use std::sync::Arc;

/// The version of Suzuri itself, and the only place it is written down.
///
/// Deliberately *not* `CARGO_PKG_VERSION`: `crates/zed/Cargo.toml` carries
/// upstream Zed's version, which upstream bumps on its own release cadence and
/// every merge brings along. Comparing a Suzuri release tag against that number
/// would compare two unrelated sequences. This constant lives in a file
/// upstream never touches, so a merge can never move it.
///
/// Bump it in the same commit that gets tagged `suzuri-v<this version>`; the
/// `check-version` job in `.github/workflows/suzuri-release.yml` enforces that
/// the two agree.
pub const SUZURI_VERSION: &str = "0.5.0";

/// The repository whose releases describe newer Suzuri builds.
const RELEASE_REPOSITORY: &str = "harrywang/suzuri";
/// `.github/workflows/suzuri-release.yml` publishes one release per `suzuri-v*` tag.
const RELEASE_TAG_PREFIX: &str = "suzuri-v";

/// The newest published build of Suzuri for one OS and CPU.
#[derive(Debug, PartialEq, Eq)]
pub struct AppRelease {
    pub version: Version,
    /// The asset the updater installs: a `.dmg`, an installer `.exe`, or a
    /// `.tar.gz`, matching what `auto_update` expects for the host OS.
    pub download_url: String,
}

/// Asks GitHub for the newest Suzuri release and the build in it for this host.
///
/// Fails, rather than returning a release page, when the release has no asset
/// for the host: the caller installs what it is handed, and cannot install a
/// web page.
pub async fn latest_app_release(
    http_client: Arc<dyn HttpClient>,
    os: &str,
    arch: &str,
) -> Result<AppRelease> {
    let release = latest_github_release(RELEASE_REPOSITORY, true, false, http_client)
        .await
        .context("could not reach GitHub to look for a newer Suzuri release")?;

    let version = parse_release_version(&release.tag_name).with_context(|| {
        format!(
            "the newest release of {RELEASE_REPOSITORY} is tagged {:?}, \
             which is not a {RELEASE_TAG_PREFIX}<version> tag",
            release.tag_name
        )
    })?;

    let download_url = download_url_for_host(&release, os, arch)
        .with_context(|| {
            format!(
                "Suzuri {version} has no build for {os} on {arch}; see {}",
                release_page_url(&version)
            )
        })?
        .to_owned();

    Ok(AppRelease {
        version,
        download_url,
    })
}

/// Turns `suzuri-v1.18.0` into `1.18.0`.
fn parse_release_version(tag_name: &str) -> Option<Version> {
    tag_name.strip_prefix(RELEASE_TAG_PREFIX)?.parse().ok()
}

/// The GitHub release page for a Suzuri version: its release notes.
pub fn release_page_url(version: &Version) -> String {
    format!("https://github.com/{RELEASE_REPOSITORY}/releases/tag/{RELEASE_TAG_PREFIX}{version}")
}

/// Where this build fetches the remote server it copies onto an SSH host.
///
/// Zed's client asks its own release index for that binary, which knows nothing
/// about Suzuri builds and, on the `dev` channel Suzuri ships on, refuses to ask
/// at all. The release workflow publishes the same `zed-remote-server-<os>-<arch>`
/// artifacts Zed does under this build's own tag, so the asset named here is
/// the server built from the same commit as the client requesting it.
pub fn remote_server_download_url(os: &str, arch: &str) -> String {
    let extension = if os == "windows" { "zip" } else { "gz" };
    format!(
        "https://github.com/{RELEASE_REPOSITORY}/releases/download/\
         {RELEASE_TAG_PREFIX}{SUZURI_VERSION}/zed-remote-server-{os}-{arch}.{extension}"
    )
}

/// Picks the asset a user on this OS and CPU should download, matching on shape
/// rather than exact filenames so that renaming an artifact in the release
/// workflow degrades to a clear error instead of installing the wrong file.
fn download_url_for_host<'a>(release: &'a GithubRelease, os: &str, arch: &str) -> Option<&'a str> {
    let matches = |asset: &GithubReleaseAsset| match os {
        "macos" => asset.name.ends_with(".dmg") && asset.name.contains(arch),
        "windows" => asset.name.ends_with(".exe") && asset.name.contains("windows"),
        "linux" => asset.name.ends_with(".tar.gz") && asset.name.contains(arch),
        _ => false,
    };

    release
        .assets
        .iter()
        .find(|asset| matches(asset))
        .map(|asset| asset.browser_download_url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{AsyncBody, FakeHttpClient, Response};

    /// Shaped like a real response from `/repos/{owner}/{repo}/releases`, so
    /// that a change to the fields `GithubRelease` requires — an upstream merge
    /// narrowing them, or GitHub dropping one — fails here rather than in the
    /// hands of users who then never hear about a release.
    const RELEASES_JSON: &str = r#"[
      {
        "tag_name": "suzuri-v0.3.0",
        "prerelease": false,
        "tarball_url": "https://api.github.test/repos/harrywang/suzuri/tarball/suzuri-v0.3.0",
        "zipball_url": "https://api.github.test/repos/harrywang/suzuri/zipball/suzuri-v0.3.0",
        "assets": [
          {
            "name": "suzuri-aarch64.dmg",
            "browser_download_url": "https://github.test/suzuri-aarch64.dmg",
            "digest": "sha256:0123456789abcdef"
          },
          {
            "name": "suzuri-x86_64.dmg",
            "browser_download_url": "https://github.test/suzuri-x86_64.dmg",
            "digest": null
          },
          {
            "name": "suzuri-windows-x86_64.exe",
            "browser_download_url": "https://github.test/suzuri-windows-x86_64.exe",
            "digest": null
          }
        ]
      }
    ]"#;

    fn fake_github(body: &'static str) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |_| async move {
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body))?)
        })
    }

    fn version(text: &str) -> Version {
        text.parse().expect("test version should be valid semver")
    }

    fn asset(name: &str) -> GithubReleaseAsset {
        GithubReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
            digest: None,
        }
    }

    fn release(tag_name: &str, asset_names: &[&str]) -> GithubRelease {
        GithubRelease {
            tag_name: tag_name.to_string(),
            pre_release: false,
            assets: asset_names.iter().copied().map(asset).collect(),
            tarball_url: String::new(),
            zipball_url: String::new(),
        }
    }

    #[test]
    fn test_parse_release_version() {
        assert_eq!(
            parse_release_version("suzuri-v1.18.0"),
            Some(Version::new(1, 18, 0))
        );
        // Zed's own tags share this repository's history but not its releases;
        // reading one as a Suzuri version would advertise the wrong download.
        assert_eq!(parse_release_version("v0.180.0"), None);
        assert_eq!(parse_release_version("suzuri-v"), None);
        assert_eq!(parse_release_version("suzuri-vnightly"), None);
    }

    #[test]
    fn test_suzuri_version_is_valid_semver() {
        // The updater falls back to Zed's version when this does not parse,
        // which would silently leave every user of that build never updating.
        assert!(SUZURI_VERSION.parse::<Version>().is_ok());
    }

    #[test]
    fn test_remote_server_download_url_names_this_builds_release() {
        assert_eq!(
            remote_server_download_url("linux", "x86_64"),
            format!(
                "https://github.com/harrywang/suzuri/releases/download/\
                 suzuri-v{SUZURI_VERSION}/zed-remote-server-linux-x86_64.gz"
            )
        );
        assert_eq!(
            remote_server_download_url("windows", "aarch64"),
            format!(
                "https://github.com/harrywang/suzuri/releases/download/\
                 suzuri-v{SUZURI_VERSION}/zed-remote-server-windows-aarch64.zip"
            )
        );
    }

    #[test]
    fn test_download_url_matches_host_asset() {
        let release = release(
            "suzuri-v1.18.0",
            &[
                "suzuri-aarch64.dmg",
                "suzuri-x86_64.dmg",
                "suzuri-windows-x86_64.exe",
                "suzuri-linux-x86_64.tar.gz",
                "suzuri-linux-aarch64.tar.gz",
            ],
        );

        assert_eq!(
            download_url_for_host(&release, "macos", "aarch64"),
            Some("https://example.test/suzuri-aarch64.dmg")
        );
        assert_eq!(
            download_url_for_host(&release, "macos", "x86_64"),
            Some("https://example.test/suzuri-x86_64.dmg")
        );
        assert_eq!(
            download_url_for_host(&release, "windows", "x86_64"),
            Some("https://example.test/suzuri-windows-x86_64.exe")
        );
        assert_eq!(
            download_url_for_host(&release, "linux", "x86_64"),
            Some("https://example.test/suzuri-linux-x86_64.tar.gz")
        );
        assert_eq!(
            download_url_for_host(&release, "linux", "aarch64"),
            Some("https://example.test/suzuri-linux-aarch64.tar.gz")
        );
    }

    #[test]
    fn test_download_url_is_absent_when_assets_do_not_match() {
        let release = release("suzuri-v1.18.0", &["suzuri-x86_64.dmg"]);

        assert_eq!(download_url_for_host(&release, "macos", "aarch64"), None);
    }

    #[test]
    fn test_release_page_url() {
        assert_eq!(
            release_page_url(&Version::new(1, 18, 0)),
            "https://github.com/harrywang/suzuri/releases/tag/suzuri-v1.18.0"
        );
    }

    #[test]
    fn test_latest_app_release_offers_the_host_download() {
        let release = block_on(latest_app_release(
            fake_github(RELEASES_JSON),
            "macos",
            "aarch64",
        ))
        .expect("a well-formed release listing should be understood");

        assert_eq!(
            release,
            AppRelease {
                version: version("0.3.0"),
                download_url: "https://github.test/suzuri-aarch64.dmg".to_string(),
            }
        );
    }

    #[test]
    fn test_latest_app_release_fails_without_a_host_asset() {
        let error = block_on(latest_app_release(
            fake_github(RELEASES_JSON),
            "linux",
            "x86_64",
        ))
        .expect_err("a release with nothing to install on this host is not an update");

        // The message is what the title bar shows, so it should say which
        // release lacks the build and where to look.
        let message = format!("{error:#}");
        assert!(message.contains("linux"), "{message}");
        assert!(message.contains("suzuri-v0.3.0"), "{message}");
    }

    #[test]
    fn test_latest_app_release_rejects_a_release_that_is_not_suzuris() {
        const ZED_RELEASE_JSON: &str = r#"[
          {
            "tag_name": "v0.180.0",
            "prerelease": false,
            "tarball_url": "https://api.github.test/tarball/v0.180.0",
            "zipball_url": "https://api.github.test/zipball/v0.180.0",
            "assets": [
              {
                "name": "Zed.dmg",
                "browser_download_url": "https://github.test/Zed.dmg",
                "digest": null
              }
            ]
          }
        ]"#;

        let error = block_on(latest_app_release(
            fake_github(ZED_RELEASE_JSON),
            "macos",
            "aarch64",
        ))
        .expect_err("a tag that is not a Suzuri release must not become an update");

        assert!(
            format!("{error:#}").contains("v0.180.0"),
            "the error should name the tag it could not read: {error:#}"
        );
    }

    #[test]
    fn test_latest_app_release_surfaces_an_unreachable_github() {
        let unreachable = FakeHttpClient::with_404_response();

        let error = block_on(latest_app_release(unreachable, "macos", "aarch64"))
            .expect_err("a failed request must not be reported as being up to date");

        assert!(
            format!("{error:#}").contains("GitHub"),
            "the error should say what could not be reached: {error:#}"
        );
    }
}
