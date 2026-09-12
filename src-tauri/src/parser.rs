use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde::Deserialize;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::settings::{normalize_quality, AppSettings};

const DOUYIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const SESSION_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DOUYIN_SESSION_REJECTED_STATUS: u16 = 444;

#[derive(Debug, Clone)]
pub struct LiveInfo {
    pub platform: String,
    pub room_id: String,
    pub anchor_name: String,
    pub room_title: String,
    pub cover_url: String,
    pub avatar_url: String,
    pub is_live: bool,
    pub stream_url: String,
}

#[derive(Debug, Clone)]
pub struct ParseError {
    message: String,
    rate_limited: bool,
    authentication_failed: bool,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            rate_limited: false,
            authentication_failed: false,
        }
    }

    fn http(status: reqwest::StatusCode) -> Self {
        let session_rejected = status.as_u16() == DOUYIN_SESSION_REJECTED_STATUS;
        Self {
            message: if session_rejected {
                "抖音 API 拒绝了当前会话: HTTP 444".to_string()
            } else {
                format!("抖音 API 返回错误: HTTP {}", status)
            },
            rate_limited: status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::FORBIDDEN
                || session_rejected,
            authentication_failed: status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
                || session_rejected,
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        self.rate_limited
    }
}

impl Display for ParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

struct ClientSession {
    proxy: String,
    client: reqwest::Client,
    ttwid: String,
    created_at: Instant,
}

pub struct DouyinParser {
    session: Mutex<Option<ClientSession>>,
}

impl DouyinParser {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }

    pub async fn parse_douyin_url(
        &self,
        url: &str,
        settings: &AppSettings,
    ) -> Result<LiveInfo, ParseError> {
        let room_id = extract_room_id(url)?;
        let mut session = self.session.lock().await;

        for attempt in 0..2 {
            let needs_session = session.as_ref().is_none_or(|current| {
                current.proxy != settings.proxy || current.created_at.elapsed() >= SESSION_TTL
            });
            if needs_session {
                *session = Some(build_session(settings).await?);
            }

            let result = request_room(
                session.as_ref().expect("session initialized"),
                &room_id,
                settings,
            )
            .await;
            match result {
                Err(error) if should_refresh_session(&error, attempt) => {
                    *session = Some(build_session(settings).await?);
                }
                result => return result,
            }
        }

        Err(ParseError::new("抖音登录会话刷新后仍然无效"))
    }
}

fn should_refresh_session(error: &ParseError, attempt: usize) -> bool {
    error.authentication_failed && attempt == 0
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    data: ApiData,
}

#[derive(Debug, Deserialize)]
struct ApiData {
    data: Vec<RoomData>,
}

#[derive(Debug, Deserialize)]
struct RoomData {
    status: Option<i64>,
    title: Option<String>,
    owner: Option<Owner>,
    cover: Option<Cover>,
    #[serde(rename = "stream_url")]
    stream_url: Option<StreamUrl>,
}

#[derive(Debug, Deserialize)]
struct Owner {
    nickname: Option<String>,
    avatar_thumb: Option<AvatarThumb>,
}

#[derive(Debug, Deserialize)]
struct AvatarThumb {
    url_list: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Cover {
    url_list: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct StreamUrl {
    #[serde(rename = "flv_pull_url")]
    flv_pull_url: Option<serde_json::Value>,
    #[serde(rename = "hls_pull_url_map")]
    hls_pull_url_map: Option<serde_json::Value>,
    // Keep optional SDK fields untyped so malformed SDK data cannot break legacy streams.
    live_core_sdk_data: Option<serde_json::Value>,
}

fn extract_room_id(url: &str) -> Result<String, ParseError> {
    let url = url.trim();

    if url.contains("live.douyin.com") {
        let path = url
            .split("live.douyin.com")
            .nth(1)
            .ok_or_else(|| ParseError::new("无法解析直播间链接"))?;
        let room_id = path
            .trim_start_matches('/')
            .split('?')
            .next()
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        if !room_id.is_empty() {
            return Ok(room_id.to_string());
        }
    }

    if url.contains("v.douyin.com") {
        return Err(ParseError::new(
            "暂不支持短链接，请复制完整的直播间链接 (live.douyin.com/...)",
        ));
    }

    Err(ParseError::new(
        "无法解析直播间ID，请输入 live.douyin.com 格式的链接",
    ))
}

fn build_client(settings: &AppSettings) -> Result<reqwest::Client, ParseError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15));

    if !settings.proxy.is_empty() {
        let proxy = reqwest::Proxy::all(&settings.proxy)
            .map_err(|error| ParseError::new(format!("代理配置无效: {}", error)))?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|error| ParseError::new(format!("创建 HTTP 客户端失败: {}", error)))
}

async fn build_session(settings: &AppSettings) -> Result<ClientSession, ParseError> {
    let client = build_client(settings)?;
    let ttwid = get_ttwid(&client).await?;
    Ok(ClientSession {
        proxy: settings.proxy.clone(),
        client,
        ttwid,
        created_at: Instant::now(),
    })
}

async fn get_ttwid(client: &reqwest::Client) -> Result<String, ParseError> {
    let response = client
        .get("https://live.douyin.com")
        .header(USER_AGENT, DOUYIN_UA)
        .send()
        .await
        .map_err(|error| ParseError::new(format!("获取 ttwid 失败: {}", error)))?;

    if !response.status().is_success() {
        return Err(ParseError::http(response.status()));
    }

    for cookie in response.headers().get_all("set-cookie") {
        let cookie = cookie.to_str().unwrap_or("");
        if cookie.starts_with("ttwid=") {
            let ttwid = cookie
                .split(';')
                .next()
                .unwrap_or("")
                .strip_prefix("ttwid=")
                .unwrap_or("");
            if !ttwid.is_empty() {
                return Ok(ttwid.to_string());
            }
        }
    }

    Err(ParseError::new("无法获取 ttwid cookie，抖音可能更新了接口"))
}

async fn request_room(
    session: &ClientSession,
    room_id: &str,
    settings: &AppSettings,
) -> Result<LiveInfo, ParseError> {
    let cookie_value = build_cookie_header(&session.ttwid, &settings.cookie);

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(DOUYIN_UA));
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://live.douyin.com/"),
    );
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&cookie_value)
            .map_err(|_| ParseError::new("构建 cookie 失败，可能包含非法字符"))?,
    );

    let api_url = format!(
        "https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web\
        &live_id=1&device_platform=web&language=zh-CN&enter_from=web_live\
        &cookie_enabled=true&browser_language=zh-CN&browser_platform=Win32\
        &browser_name=Chrome&browser_version=120&web_rid={}",
        room_id
    );

    let response = session
        .client
        .get(&api_url)
        .headers(headers)
        .send()
        .await
        .map_err(|error| ParseError::new(format!("请求抖音 API 失败: {}", error)))?;

    if !response.status().is_success() {
        return Err(ParseError::http(response.status()));
    }

    let body = response
        .text()
        .await
        .map_err(|error| ParseError::new(format!("读取响应失败: {}", error)))?;
    let preview: String = body.chars().take(100).collect();
    let api_response: ApiResponse = serde_json::from_str(&body).map_err(|error| {
        ParseError::new(format!("解析 API 响应失败: {} (响应: {})", error, preview))
    })?;
    let room = api_response
        .data
        .data
        .into_iter()
        .next()
        .ok_or_else(|| ParseError::new("API 返回的房间数据为空，直播间可能不存在"))?;

    let is_live = room.status == Some(2);
    let anchor_name = room
        .owner
        .as_ref()
        .and_then(|owner| owner.nickname.clone())
        .unwrap_or_default();
    let room_title = room.title.unwrap_or_default();
    let cover_url = room
        .cover
        .as_ref()
        .and_then(|cover| cover.url_list.as_ref())
        .and_then(|urls| urls.first().cloned())
        .unwrap_or_default();
    let avatar_url = room
        .owner
        .as_ref()
        .and_then(|owner| owner.avatar_thumb.as_ref())
        .and_then(|avatar| avatar.url_list.as_ref())
        .and_then(|urls| urls.first().cloned())
        .unwrap_or_default();
    let stream_url = room
        .stream_url
        .as_ref()
        .map(|stream_url| get_best_stream_url(stream_url, &settings.quality))
        .unwrap_or_default();

    Ok(LiveInfo {
        platform: "douyin".to_string(),
        room_id: room_id.to_string(),
        anchor_name,
        room_title,
        cover_url,
        avatar_url,
        is_live,
        stream_url,
    })
}

fn build_cookie_header(ttwid: &str, user_cookie: &str) -> String {
    let extra_cookies = user_cookie
        .split(';')
        .map(str::trim)
        .filter(|cookie| !cookie.is_empty())
        .filter(|cookie| {
            cookie
                .split_once('=')
                .map(|(name, _)| !name.trim().eq_ignore_ascii_case("ttwid"))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();

    if extra_cookies.is_empty() {
        format!("ttwid={}", ttwid)
    } else {
        format!("ttwid={}; {}", ttwid, extra_cookies.join("; "))
    }
}

fn get_best_stream_url(stream_url: &StreamUrl, preferred: &str) -> String {
    // Descending quality; preserve the four existing settings values.
    const QUALITIES: [(&str, &str); 5] = [
        ("ORIGIN", "origin"),
        ("FULL_HD1", "uhd"),
        ("HD1", "hd"),
        ("SD2", "sd"),
        ("SD1", "ld"),
    ];
    let preferred = normalize_quality(preferred);
    let preferred_index = QUALITIES
        .iter()
        .position(|(quality, _)| *quality == preferred)
        .unwrap_or(0);
    let sdk_stream_data = stream_url
        .live_core_sdk_data
        .as_ref()
        .and_then(|value| value.get("pull_data"))
        .and_then(|value| value.get("stream_data"))
        .and_then(|value| match value {
            serde_json::Value::String(json) => serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .map(Cow::Owned),
            serde_json::Value::Object(_) => Some(Cow::Borrowed(value)),
            _ => None,
        });

    // Try the requested tier and lower tiers before the nearest higher tier.
    for &(quality, sdk_key) in QUALITIES[preferred_index..]
        .iter()
        .chain(QUALITIES[..preferred_index].iter().rev())
    {
        let sdk_main = sdk_stream_data
            .as_deref()
            .and_then(|value| value.get("data"))
            .and_then(|value| value.get(sdk_key))
            .and_then(|value| value.get("main"));
        let candidates = [
            sdk_main.and_then(|value| value.get("flv")),
            sdk_main.and_then(|value| value.get("hls")),
            stream_url
                .flv_pull_url
                .as_ref()
                .and_then(|value| value.get(quality)),
            stream_url
                .hls_pull_url_map
                .as_ref()
                .and_then(|value| value.get(quality)),
        ];
        if let Some(url) = candidates
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
            .map(str::trim)
            .find(|url| !url.is_empty())
        {
            return url.to_string();
        }
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::{
        build_cookie_header, get_best_stream_url, should_refresh_session, ParseError, StreamUrl,
        DOUYIN_SESSION_REJECTED_STATUS,
    };
    use serde_json::{json, Value};

    fn parse_streams(value: Value) -> StreamUrl {
        serde_json::from_value(value).expect("deserialize sanitized stream response")
    }

    fn sdk_fixture() -> Value {
        json!({"data": {
            "origin": {"main": {"flv": "https://example.com/origin.flv"}},
            "uhd": {"main": {"flv": "https://example.com/uhd.flv"}},
            "hd": {"main": {"flv": "https://example.com/hd.flv"}},
            "sd": {"main": {"flv": "https://example.com/sd.flv"}},
            "ld": {"main": {"flv": "https://example.com/ld.flv"}}
        }})
    }

    #[test]
    fn selects_all_five_sdk_tiers_from_string_or_object() {
        let sdk_data = sdk_fixture();
        for data in [sdk_data.clone(), Value::String(sdk_data.to_string())] {
            let streams = parse_streams(json!({
                "live_core_sdk_data": {"pull_data": {"stream_data": data}},
                "flv_pull_url": {"FULL_HD1": "https://example.com/legacy-blue.flv"}
            }));
            for (quality, expected) in [
                ("ORIGIN", "https://example.com/origin.flv"),
                ("FULL_HD1", "https://example.com/uhd.flv"),
                ("HD1", "https://example.com/hd.flv"),
                ("SD2", "https://example.com/sd.flv"),
                ("SD1", "https://example.com/ld.flv"),
            ] {
                assert_eq!(
                    get_best_stream_url(&streams, quality),
                    expected,
                    "{quality}"
                );
            }
        }
    }

    #[test]
    fn preserves_legacy_tiers_and_falls_back_from_origin() {
        for transport in ["flv_pull_url", "hls_pull_url_map"] {
            let streams = parse_streams(json!({transport: {
                "FULL_HD1": "https://example.com/blue",
                "HD1": "https://example.com/super",
                "SD2": "https://example.com/high",
                "SD1": "https://example.com/standard"
            }}));
            for (quality, expected) in [
                ("ORIGIN", "https://example.com/blue"),
                ("FULL_HD1", "https://example.com/blue"),
                ("HD1", "https://example.com/super"),
                ("SD2", "https://example.com/high"),
                ("SD1", "https://example.com/standard"),
            ] {
                assert_eq!(
                    get_best_stream_url(&streams, quality),
                    expected,
                    "{transport}/{quality}"
                );
            }
        }
    }

    #[test]
    fn follows_fallback_order_for_every_available_tier_combination() {
        let tiers = ["origin", "uhd", "hd", "sd", "ld"];
        let orders = [
            ("ORIGIN", ["origin", "uhd", "hd", "sd", "ld"]),
            ("FULL_HD1", ["uhd", "hd", "sd", "ld", "origin"]),
            ("HD1", ["hd", "sd", "ld", "uhd", "origin"]),
            ("SD2", ["sd", "ld", "hd", "uhd", "origin"]),
            ("SD1", ["ld", "sd", "hd", "uhd", "origin"]),
        ];
        for mask in 0..32 {
            let mut data = sdk_fixture();
            let available = data["data"].as_object_mut().unwrap();
            for (index, tier) in tiers.iter().enumerate() {
                if mask & (1 << index) == 0 {
                    available.remove(*tier);
                }
            }
            for (quality, order) in orders {
                let expected = order
                    .iter()
                    .find(|tier| data["data"].get(**tier).is_some())
                    .map(|tier| format!("https://example.com/{tier}.flv"))
                    .unwrap_or_default();
                let streams = parse_streams(json!({
                    "live_core_sdk_data": {"pull_data": {"stream_data": data}}
                }));
                assert_eq!(
                    get_best_stream_url(&streams, quality),
                    expected,
                    "mask={mask}, {quality}"
                );
            }
        }
    }

    #[test]
    fn exhausts_same_tier_sources_before_using_lower_quality() {
        let mut response = json!({
            "live_core_sdk_data": {"pull_data": {"stream_data": {"data": {
                "hd": {"main": {
                    "flv": "https://example.com/sdk-hd.flv",
                    "hls": "https://example.com/sdk-hd.m3u8"
                }},
                "sd": {"main": {"flv": "https://example.com/sdk-sd.flv"}}
            }}}},
            "flv_pull_url": {"HD1": "https://example.com/legacy-hd.flv"},
            "hls_pull_url_map": {"HD1": "https://example.com/legacy-hd.m3u8"}
        });
        for (field, expected) in [
            (
                "/live_core_sdk_data/pull_data/stream_data/data/hd/main/flv",
                "https://example.com/sdk-hd.flv",
            ),
            (
                "/live_core_sdk_data/pull_data/stream_data/data/hd/main/hls",
                "https://example.com/sdk-hd.m3u8",
            ),
            ("/flv_pull_url/HD1", "https://example.com/legacy-hd.flv"),
            (
                "/hls_pull_url_map/HD1",
                "https://example.com/legacy-hd.m3u8",
            ),
        ] {
            assert_eq!(
                get_best_stream_url(&parse_streams(response.clone()), "HD1"),
                expected
            );
            *response.pointer_mut(field).unwrap() = json!(" \t");
        }
        assert_eq!(
            get_best_stream_url(&parse_streams(response), "HD1"),
            "https://example.com/sdk-sd.flv"
        );
    }

    #[test]
    fn malformed_sdk_data_does_not_block_legacy_streams() {
        for sdk in [
            Value::Null,
            json!("unexpected SDK type"),
            json!({"pull_data": []}),
            json!({"pull_data": {"stream_data": "{broken json"}}),
            json!({"pull_data": {"stream_data": 42}}),
            json!({"pull_data": {"stream_data": "null"}}),
            json!({"pull_data": {"stream_data": {"data": {"hd": {"main": {"flv": {}, "hls": null}}}}}}),
        ] {
            let streams = parse_streams(json!({
                "live_core_sdk_data": sdk,
                "hls_pull_url_map": {"HD1": "https://example.com/legacy.m3u8"}
            }));
            assert_eq!(
                get_best_stream_url(&streams, "HD1"),
                "https://example.com/legacy.m3u8"
            );
        }
    }

    #[test]
    fn ignores_empty_urls_and_internal_sdk_tiers() {
        let streams = parse_streams(json!({
            "live_core_sdk_data": {"pull_data": {"stream_data": {"data": {
                "origin": {"main": {"flv": "", "hls": "  "}},
                "hd": {"main": {"flv": false}},
                "md": {"main": {"flv": "https://example.com/internal-low.flv"}},
                "ao": {"main": {"flv": "https://example.com/audio.flv"}}
            }}}},
            "flv_pull_url": {"FULL_HD1": null, "SD2": ""},
            "hls_pull_url_map": {"SD1": "\t"}
        }));
        assert!(get_best_stream_url(&streams, "ORIGIN").is_empty());
        assert!(get_best_stream_url(&parse_streams(json!({})), "HD1").is_empty());
    }

    #[test]
    fn empty_and_unknown_preferences_use_origin() {
        let streams = parse_streams(json!({
            "live_core_sdk_data": {"pull_data": {"stream_data": sdk_fixture()}}
        }));
        for preference in ["", " \t\n", "invalid-quality"] {
            assert_eq!(
                get_best_stream_url(&streams, preference),
                "https://example.com/origin.flv"
            );
        }
    }

    #[test]
    fn treats_http_444_as_a_rejected_session_and_rate_limit() {
        let status = reqwest::StatusCode::from_u16(DOUYIN_SESSION_REJECTED_STATUS)
            .expect("444 is a representable HTTP status");
        let error = ParseError::http(status);

        assert!(error.authentication_failed);
        assert!(error.is_rate_limited());
        assert!(should_refresh_session(&error, 0));
        assert!(!should_refresh_session(&error, 1));
        assert_eq!(error.to_string(), "抖音 API 拒绝了当前会话: HTTP 444");
    }

    #[test]
    fn fresh_ttwid_cannot_be_overridden_by_saved_cookie() {
        assert_eq!(
            build_cookie_header(
                "fresh-token",
                "__ac_nonce=nonce; ttwid=stale-token; sessionid=session"
            ),
            "ttwid=fresh-token; __ac_nonce=nonce; sessionid=session"
        );
        assert_eq!(
            build_cookie_header("fresh-token", "TTWID=stale-token"),
            "ttwid=fresh-token"
        );
    }
}
