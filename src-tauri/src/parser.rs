use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::settings::AppSettings;

const DOUYIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const SESSION_TTL: Duration = Duration::from_secs(6 * 60 * 60);

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
        Self {
            message: format!("抖音 API 返回错误: HTTP {}", status),
            rate_limited: status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::FORBIDDEN,
            authentication_failed: status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN,
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
                Err(error) if error.authentication_failed && attempt == 0 => {
                    *session = Some(build_session(settings).await?);
                }
                result => return result,
            }
        }

        Err(ParseError::new("抖音登录会话刷新后仍然无效"))
    }
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
    let mut cookie_value = format!("ttwid={}", session.ttwid);
    if !settings.cookie.is_empty() {
        cookie_value = format!("{}; {}", cookie_value, settings.cookie);
    }

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
    let preferred_quality = if settings.quality.is_empty() {
        "HD1"
    } else {
        &settings.quality
    };
    let stream_url = room
        .stream_url
        .as_ref()
        .map(|stream_url| get_best_stream_url(stream_url, preferred_quality))
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

fn get_best_stream_url(stream_url: &StreamUrl, preferred: &str) -> String {
    let fallback_order: Vec<&str> = match preferred {
        "FULL_HD1" => vec!["FULL_HD1", "HD1", "SD1", "SD2"],
        "SD1" => vec!["SD1", "SD2", "HD1", "FULL_HD1"],
        "SD2" => vec!["SD2", "SD1", "HD1", "FULL_HD1"],
        _ => vec!["HD1", "FULL_HD1", "SD1", "SD2"],
    };

    if let Some(object) = stream_url
        .flv_pull_url
        .as_ref()
        .and_then(|value| value.as_object())
    {
        for quality in &fallback_order {
            if let Some(url) = object.get(*quality).and_then(|value| value.as_str()) {
                if !url.is_empty() {
                    return url.to_string();
                }
            }
        }
    }

    if let Some(object) = stream_url
        .hls_pull_url_map
        .as_ref()
        .and_then(|value| value.as_object())
    {
        for quality in &fallback_order {
            if let Some(url) = object.get(*quality).and_then(|value| value.as_str()) {
                if !url.is_empty() {
                    return url.to_string();
                }
            }
        }
    }

    String::new()
}
