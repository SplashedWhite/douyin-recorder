use crate::settings::AppSettings;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::Duration;

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/SplashedWhite/douyin-recorder/releases/latest";
const GITHUB_LATEST_RELEASE_URL: &str =
    "https://github.com/SplashedWhite/douyin-recorder/releases/latest";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

pub async fn check_for_update(
    current_version: &Version,
    settings: &AppSettings,
) -> Result<Option<UpdateInfo>, String> {
    check_with_provider(settings, current_version, || async {
        fetch_latest_release(settings, current_version).await
    })
    .await
}

async fn check_with_provider<F, Fut>(
    settings: &AppSettings,
    current_version: &Version,
    provider: F,
) -> Result<Option<UpdateInfo>, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Option<GitHubRelease>, String>>,
{
    if !settings.notify_updates {
        return Ok(None);
    }

    let Some(release) = provider().await? else {
        return Ok(None);
    };
    Ok(evaluate_release(current_version, &release))
}

async fn fetch_latest_release(
    settings: &AppSettings,
    current_version: &Version,
) -> Result<Option<GitHubRelease>, String> {
    let mut client_builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10));
    if !settings.proxy.is_empty() {
        let proxy = reqwest::Proxy::all(&settings.proxy)
            .map_err(|error| format!("更新检查代理配置无效: {}", error))?;
        client_builder = client_builder.proxy(proxy);
    }
    let client = client_builder
        .build()
        .map_err(|error| format!("创建更新检查客户端失败: {}", error))?;

    let response = client
        .get(GITHUB_LATEST_RELEASE_API)
        .header(
            reqwest::header::USER_AGENT,
            format!(
                "douyin-recorder/{} (+https://github.com/SplashedWhite/douyin-recorder)",
                current_version
            ),
        )
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|error| format!("请求 GitHub 最新版本失败: {}", error))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "GitHub 最新版本接口返回 HTTP {}",
            response.status()
        ));
    }

    response
        .json::<GitHubRelease>()
        .await
        .map(Some)
        .map_err(|error| format!("解析 GitHub 最新版本失败: {}", error))
}

fn evaluate_release(current_version: &Version, release: &GitHubRelease) -> Option<UpdateInfo> {
    if release.draft || release.prerelease {
        return None;
    }
    let latest_version = parse_release_version(&release.tag_name)?;
    if !latest_version.pre.is_empty() || latest_version <= *current_version {
        return None;
    }

    Some(UpdateInfo {
        current_version: current_version.to_string(),
        latest_version: latest_version.to_string(),
        release_url: GITHUB_LATEST_RELEASE_URL.to_string(),
    })
}

fn parse_release_version(tag_name: &str) -> Option<Version> {
    let tag_name = tag_name.trim();
    let version = tag_name
        .strip_prefix('v')
        .or_else(|| tag_name.strip_prefix('V'))
        .unwrap_or(tag_name);
    Version::parse(version).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        check_with_provider, evaluate_release, parse_release_version, GitHubRelease, UpdateInfo,
        GITHUB_LATEST_RELEASE_URL,
    };
    use crate::settings::AppSettings;
    use semver::Version;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn release(tag_name: &str) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag_name.to_string(),
            draft: false,
            prerelease: false,
        }
    }

    #[test]
    fn parses_release_tags_with_or_without_v_prefix() {
        assert_eq!(parse_release_version("v0.1.6"), Some(Version::new(0, 1, 6)));
        assert_eq!(parse_release_version("0.1.6"), Some(Version::new(0, 1, 6)));
        assert_eq!(parse_release_version("V0.1.6"), Some(Version::new(0, 1, 6)));
        assert_eq!(parse_release_version("release-0.1.6"), None);
    }

    #[test]
    fn only_reports_newer_stable_releases() {
        let current = Version::new(0, 1, 5);
        assert_eq!(
            evaluate_release(&current, &release("v0.1.6")),
            Some(UpdateInfo {
                current_version: "0.1.5".to_string(),
                latest_version: "0.1.6".to_string(),
                release_url: GITHUB_LATEST_RELEASE_URL.to_string(),
            })
        );
        assert_eq!(evaluate_release(&current, &release("v0.1.5")), None);
        assert_eq!(evaluate_release(&current, &release("v0.1.4")), None);
        assert_eq!(evaluate_release(&current, &release("invalid")), None);

        let mut prerelease = release("v0.1.6-beta.1");
        prerelease.prerelease = true;
        assert_eq!(evaluate_release(&current, &prerelease), None);
        let mut draft = release("v0.1.6");
        draft.draft = true;
        assert_eq!(evaluate_release(&current, &draft), None);
    }

    #[tokio::test]
    async fn disabled_setting_skips_provider_completely() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);
        let settings = AppSettings {
            notify_updates: false,
            ..AppSettings::default()
        };

        let result = check_with_provider(&settings, &Version::new(0, 1, 5), move || {
            provider_calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(Some(release("v0.1.6"))) }
        })
        .await
        .expect("disabled update check succeeds");

        assert_eq!(result, None);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn enabled_setting_calls_provider_once_and_handles_no_release() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);

        let result =
            check_with_provider(&AppSettings::default(), &Version::new(0, 1, 5), move || {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(None) }
            })
            .await
            .expect("empty release response succeeds");

        assert_eq!(result, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_failures_are_returned_without_update_state() {
        let settings = AppSettings::default();
        for message in ["timeout", "HTTP 403", "HTTP 429", "invalid response"] {
            let result = check_with_provider(&settings, &Version::new(0, 1, 5), || async {
                Err(message.to_string())
            })
            .await;
            assert_eq!(result, Err(message.to_string()));
        }
    }
}
