use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde::Deserialize;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::recording_log::RecordingLogger;
use crate::settings::{normalize_quality, AppSettings};

const DOUYIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const SESSION_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DOUYIN_SESSION_REJECTED_STATUS: u16 = 444;
const SESSION_URL: &str = "https://live.douyin.com";
const ROOM_API_URL: &str = "https://live.douyin.com/webcast/room/web/enter/";

#[derive(Clone, Copy)]
pub enum RequestSource {
    AddRoom,
    ManualRefresh,
    ManualStart,
    AutoCheck,
    RecordingVerification,
}

impl RequestSource {
    fn label(self) -> &'static str {
        match self {
            Self::AddRoom => "添加房间",
            Self::ManualRefresh => "手动刷新",
            Self::ManualStart => "手动开始录制",
            Self::AutoCheck => "自动检测",
            Self::RecordingVerification => "录制结束复核",
        }
    }
}

struct ApiTrace {
    started_at: chrono::DateTime<chrono::Local>,
    http_status: Option<u16>,
    body: Option<String>,
    body_error: Option<String>,
    stage: &'static str,
    conclusion: &'static str,
}

impl ApiTrace {
    fn new() -> Self {
        Self {
            started_at: chrono::Local::now(),
            http_status: None,
            body: None,
            body_error: None,
            stage: "session",
            conclusion: "检测失败",
        }
    }

    fn record(
        self,
        room_id: &str,
        source: RequestSource,
        attempt: usize,
        result: &Result<LiveInfo, ParseError>,
    ) -> serde_json::Value {
        let (response_format, response) = match self.body {
            Some(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(value) => ("json", value),
                Err(_) => ("text", serde_json::Value::String(body)),
            },
            None => ("none", serde_json::Value::Null),
        };
        serde_json::json!({
            "started_at": self.started_at.to_rfc3339(),
            "finished_at": chrono::Local::now().to_rfc3339(),
            "platform_room_id": room_id, "source": source.label(), "attempt": attempt,
            "http_status": self.http_status, "stage": self.stage,
            "result": match result { Ok(info) if info.is_live => "live", Ok(_) => "offline", Err(_) => "failed" },
            "conclusion": self.conclusion, "response_format": response_format, "response": response,
            "error": result.as_ref().err().map(ToString::to_string),
            "response_read_error": self.body_error,
        })
    }
}

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
    logger: Option<Arc<RecordingLogger>>,
}

impl DouyinParser {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            logger: crate::recording_log::logger(),
        }
    }

    pub async fn parse_douyin_url(
        &self,
        url: &str,
        settings: &AppSettings,
        source: RequestSource,
    ) -> Result<LiveInfo, ParseError> {
        self.parse_with_endpoints(url, settings, source, SESSION_URL, ROOM_API_URL)
            .await
    }

    // Endpoints are supplied internally so tests exercise real HTTP without contacting Douyin.
    async fn parse_with_endpoints(
        &self,
        url: &str,
        settings: &AppSettings,
        source: RequestSource,
        session_url: &str,
        room_api_url: &str,
    ) -> Result<LiveInfo, ParseError> {
        let room_id = extract_room_id(url)?;
        let mut session = self.session.lock().await;

        for attempt in 0..2 {
            let mut trace = self
                .logger
                .as_ref()
                .filter(|logger| logger.api_enabled())
                .map(|_| ApiTrace::new());
            let mut room_requested = false;
            let result = async {
                let needs_session = session.as_ref().is_none_or(|current| {
                    current.proxy != settings.proxy || current.created_at.elapsed() >= SESSION_TTL
                });
                if needs_session {
                    *session = Some(build_session(settings, session_url).await?);
                }
                room_requested = true;
                request_room(
                    session.as_ref().expect("session initialized"),
                    &room_id,
                    settings,
                    room_api_url,
                    &mut trace,
                )
                .await
            }
            .await;
            if let (Some(logger), Some(trace)) = (&self.logger, trace) {
                logger.write_api(trace.record(&room_id, source, attempt + 1, &result));
            }
            match result {
                Err(error) if room_requested && should_refresh_session(&error, attempt) => {
                    // Build the replacement at the start of the next (also logged) attempt.
                    *session = None;
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

async fn build_session(
    settings: &AppSettings,
    session_url: &str,
) -> Result<ClientSession, ParseError> {
    let client = build_client(settings)?;
    let ttwid = get_ttwid(&client, session_url).await?;
    Ok(ClientSession {
        proxy: settings.proxy.clone(),
        client,
        ttwid,
        created_at: Instant::now(),
    })
}

async fn get_ttwid(client: &reqwest::Client, session_url: &str) -> Result<String, ParseError> {
    let response = client
        .get(session_url)
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
    room_api_url: &str,
    trace: &mut Option<ApiTrace>,
) -> Result<LiveInfo, ParseError> {
    if let Some(trace) = trace {
        trace.stage = "request";
    }
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
        "{}?aid=6383&app_name=douyin_web\
        &live_id=1&device_platform=web&language=zh-CN&enter_from=web_live\
        &cookie_enabled=true&browser_language=zh-CN&browser_platform=Win32\
        &browser_name=Chrome&browser_version=120&web_rid={}",
        room_api_url, room_id
    );

    let response = session
        .client
        .get(&api_url)
        .headers(headers)
        .send()
        .await
        .map_err(|error| ParseError::new(format!("请求抖音 API 失败: {}", error)))?;

    let status = response.status();
    if let Some(trace) = trace {
        trace.http_status = Some(status.as_u16());
    }
    if !status.is_success() && trace.is_none() {
        return Err(ParseError::http(status));
    }
    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            let message = format!("读取响应失败: {}", error);
            if let Some(trace) = trace {
                trace.body_error = Some(message.clone());
            }
            // Reading an error body is diagnostic only; preserve HTTP retry classification.
            return Err(if status.is_success() {
                ParseError::new(message)
            } else {
                ParseError::http(status)
            });
        }
    };
    let parsed = if status.is_success() {
        parse_room_response_with_conclusion(&body, room_id, &settings.quality)
    } else {
        Err(ParseError::http(status))
    };
    if let Some(trace) = trace {
        trace.body = Some(body);
        if let Ok((_, conclusion)) = &parsed {
            trace.conclusion = conclusion;
        }
    }
    parsed.map(|(info, _)| info)
}

#[cfg(test)]
fn parse_room_response(body: &str, room_id: &str, quality: &str) -> Result<LiveInfo, ParseError> {
    parse_room_response_with_conclusion(body, room_id, quality).map(|(info, _)| info)
}

fn parse_room_response_with_conclusion(
    body: &str,
    room_id: &str,
    quality: &str,
) -> Result<(LiveInfo, &'static str), ParseError> {
    let preview: String = body.chars().take(100).collect();
    let api_response: ApiResponse = match serde_json::from_str(body) {
        Ok(response) => response,
        Err(error) => {
            // Finished rooms can return a business message instead of data.data.
            // Only this explicit signal confirms offline; other errors stay errors.
            let finished = serde_json::from_str::<serde_json::Value>(body).is_ok_and(|response| {
                response
                    .pointer("/data/message")
                    .and_then(|value| value.as_str())
                    == Some("room has finished")
            });
            if finished {
                return Ok((
                    LiveInfo {
                        platform: "douyin".to_string(),
                        room_id: room_id.to_string(),
                        anchor_name: String::new(),
                        room_title: String::new(),
                        cover_url: String::new(),
                        avatar_url: String::new(),
                        is_live: false,
                        stream_url: String::new(),
                    },
                    "room has finished 明确下播",
                ));
            }
            return Err(ParseError::new(format!(
                "解析 API 响应失败: {} (响应: {})",
                error, preview
            )));
        }
    };
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
        .map(|stream_url| get_best_stream_url(stream_url, quality))
        .unwrap_or_default();

    Ok((
        LiveInfo {
            platform: "douyin".to_string(),
            room_id: room_id.to_string(),
            anchor_name,
            room_title,
            cover_url,
            avatar_url,
            is_live,
            stream_url,
        },
        if is_live {
            "房间状态显示直播中"
        } else {
            "房间状态显示下播"
        },
    ))
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
        build_cookie_header, get_best_stream_url, parse_room_response, should_refresh_session,
        ParseError, StreamUrl, DOUYIN_SESSION_REJECTED_STATUS,
    };
    use serde_json::{json, Value};

    fn mock_response(status: u16, body: &str, session: bool) -> String {
        format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{}\r\n{body}",
            body.len(), if session { "Set-Cookie: ttwid=test-session; Path=/\r\n" } else { "" })
    }

    async fn mock_server(responses: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let byte =
                        tokio::time::timeout(std::time::Duration::from_secs(5), socket.read_u8())
                            .await
                            .unwrap()
                            .unwrap();
                    request.push(byte);
                }
                requests.push(String::from_utf8(request).unwrap());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            requests
        });
        (base, server)
    }

    fn diagnostic_parser(
        enabled: bool,
    ) -> (
        tempfile::TempDir,
        super::DouyinParser,
        std::sync::Arc<crate::recording_log::RecordingLogger>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let logger = std::sync::Arc::new(crate::recording_log::RecordingLogger::new(
            dir.path().join("logs"),
        ));
        logger.apply_settings(&crate::settings::AppSettings {
            api_log_enabled: enabled,
            ..Default::default()
        });
        let parser = super::DouyinParser {
            session: tokio::sync::Mutex::new(None),
            logger: Some(logger.clone()),
        };
        (dir, parser, logger)
    }

    fn api_records(dir: &tempfile::TempDir) -> Vec<Value> {
        std::fs::read_to_string(dir.path().join("logs/douyin-api.log"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn logs_actual_http_responses_and_conclusions_without_changing_results() {
        use super::RequestSource::*;
        let cases = [
            (
                200,
                r#"{"data":{"data":[{"status":2}]}}"#,
                "live",
                "房间状态显示直播中",
                AddRoom,
            ),
            (
                200,
                r#"{"data":{"data":[{"status":4}]}}"#,
                "offline",
                "房间状态显示下播",
                ManualRefresh,
            ),
            (
                200,
                r#"{"data":{"message":"room has finished","prompts":"直播已结束"}}"#,
                "offline",
                "room has finished 明确下播",
                RecordingVerification,
            ),
            (
                200,
                r#"{"data":{"message":"room not found"}}"#,
                "failed",
                "检测失败",
                ManualStart,
            ),
            (
                200,
                "<html>temporary error</html>",
                "failed",
                "检测失败",
                AutoCheck,
            ),
            (
                429,
                r#"{"data":{"message":"too many requests"}}"#,
                "failed",
                "检测失败",
                AutoCheck,
            ),
            (
                500,
                r#"{"data":{"message":"room has finished"}}"#,
                "failed",
                "检测失败",
                RecordingVerification,
            ),
        ];
        for (status, body, expected, conclusion, source) in cases {
            let (dir, parser, _) = diagnostic_parser(true);
            let (base, server) = mock_server(vec![
                mock_response(200, "{}", true),
                mock_response(status, body, false),
            ])
            .await;
            let result = parser
                .parse_with_endpoints(
                    "https://live.douyin.com/123",
                    &Default::default(),
                    source,
                    &base,
                    &format!("{base}/room"),
                )
                .await;
            match expected {
                "live" => assert!(result.unwrap().is_live),
                "offline" => assert!(!result.unwrap().is_live),
                _ => {
                    let error = result.unwrap_err();
                    assert_eq!(error.is_rate_limited(), status == 429);
                    if status != 200 {
                        assert!(error.to_string().contains(&status.to_string()));
                    }
                }
            }
            let requests = server.await.unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[1].starts_with("GET /room?aid=6383"));
            let records = api_records(&dir);
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record["http_status"], status);
            assert_eq!(record["source"], source.label());
            assert_eq!(record["platform_room_id"], "123");
            assert_eq!(record["attempt"], 1);
            assert_eq!(record["result"], expected);
            assert_eq!(record["conclusion"], conclusion);
            let start =
                chrono::DateTime::parse_from_rfc3339(record["started_at"].as_str().unwrap())
                    .unwrap();
            let end = chrono::DateTime::parse_from_rfc3339(record["finished_at"].as_str().unwrap())
                .unwrap();
            assert!(end >= start);
            match serde_json::from_str::<Value>(body) {
                Ok(response) => {
                    assert_eq!(record["response"], response);
                    assert_eq!(record["response_format"], "json");
                }
                Err(_) => {
                    assert_eq!(record["response"], body);
                    assert_eq!(record["response_format"], "text");
                }
            }
        }
    }

    #[tokio::test]
    async fn session_retry_has_separate_records_and_keeps_original_http_error_on_body_failure() {
        for recover in [true, false] {
            let (dir, parser, _) = diagnostic_parser(true);
            let broken =
                "HTTP/1.1 444 Rejected\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort"
                    .to_string();
            let finished = r#"{"data":{"message":"room has finished"}}"#;
            let (base, server) = mock_server(vec![
                mock_response(200, "{}", true),
                broken.clone(),
                mock_response(200, "{}", true),
                if recover {
                    mock_response(200, finished, false)
                } else {
                    broken
                },
            ])
            .await;
            let result = parser
                .parse_with_endpoints(
                    "https://live.douyin.com/123",
                    &Default::default(),
                    super::RequestSource::RecordingVerification,
                    &base,
                    &format!("{base}/room"),
                )
                .await;
            if recover {
                assert!(!result.unwrap().is_live);
            } else {
                let error = result.unwrap_err();
                assert!(error.is_rate_limited());
                assert!(error.authentication_failed);
                assert!(error.to_string().contains("HTTP 444"));
            }
            assert_eq!(server.await.unwrap().len(), 4);
            let records = api_records(&dir);
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["attempt"], 1);
            assert_eq!(records[0]["http_status"], 444);
            assert_eq!(records[0]["result"], "failed");
            assert!(records[0]["response_read_error"]
                .as_str()
                .unwrap()
                .contains("读取响应失败"));
            assert!(records[0]["error"].as_str().unwrap().contains("HTTP 444"));
            assert_eq!(records[1]["attempt"], 2);
            assert_eq!(
                records[1]["result"],
                if recover { "offline" } else { "failed" }
            );
        }
    }

    #[tokio::test]
    async fn records_connection_and_success_body_read_failures() {
        for read_failure in [false, true] {
            let (dir, parser, _) = diagnostic_parser(true);
            let mut responses = vec![mock_response(200, "{}", true)];
            if read_failure {
                responses.push(
                    "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort"
                        .into(),
                );
            }
            let (base, server) = mock_server(responses).await;
            let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let unavailable = format!("http://{}/room", unused.local_addr().unwrap());
            drop(unused);
            let endpoint = if read_failure {
                format!("{base}/room")
            } else {
                unavailable
            };
            let result = parser
                .parse_with_endpoints(
                    "https://live.douyin.com/123",
                    &Default::default(),
                    super::RequestSource::ManualRefresh,
                    &base,
                    &endpoint,
                )
                .await;
            assert!(result.is_err());
            server.await.unwrap();
            let records = api_records(&dir);
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["result"], "failed");
            assert_eq!(records[0]["response_format"], "none");
            assert_eq!(
                records[0]["http_status"],
                if read_failure {
                    json!(200)
                } else {
                    Value::Null
                }
            );
        }
    }

    #[tokio::test]
    async fn disabled_or_unwritable_api_log_does_not_change_room_result() {
        for enabled in [false, true] {
            let (dir, parser, logger) = diagnostic_parser(enabled);
            let log_path = dir.path().join("logs/douyin-api.log");
            if enabled {
                std::fs::create_dir_all(&log_path).unwrap();
            }
            let (base, server) = mock_server(vec![
                mock_response(200, "{}", true),
                mock_response(200, r#"{"data":{"data":[{"status":2}]}}"#, false),
            ])
            .await;
            let result = parser
                .parse_with_endpoints(
                    "https://live.douyin.com/123",
                    &Default::default(),
                    super::RequestSource::ManualStart,
                    &base,
                    &format!("{base}/room"),
                )
                .await;
            assert!(result.unwrap().is_live);
            assert_eq!(server.await.unwrap().len(), 2);
            if enabled {
                assert!(logger.info().api_last_error.is_some());
            } else {
                assert!(!log_path.exists());
            }
        }
    }

    #[tokio::test]
    async fn session_initialization_failure_is_logged_without_adding_a_retry() {
        let (dir, parser, _) = diagnostic_parser(true);
        let (base, server) = mock_server(vec![mock_response(403, "{}", false)]).await;
        let result = parser
            .parse_with_endpoints(
                "https://live.douyin.com/123",
                &Default::default(),
                super::RequestSource::ManualRefresh,
                &base,
                &format!("{base}/room"),
            )
            .await;
        let error = result.unwrap_err();
        assert!(error.authentication_failed);
        assert!(error.to_string().contains("HTTP 403"));
        assert_eq!(server.await.unwrap().len(), 1);
        let records = api_records(&dir);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["stage"], "session");
        assert_eq!(records[0]["result"], "failed");
        assert_eq!(records[0]["response_format"], "none");
    }

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
    fn room_finished_response_is_offline() {
        // The reported response has a business message instead of data.data.
        let body = r#"{"data":{"message":"room has finished","prompts":"直播已结束"},"extra":{"now":1791390238431}}"#;
        let info = parse_room_response(body, "123", "ORIGIN").unwrap();

        assert_eq!(info.platform, "douyin");
        assert_eq!(info.room_id, "123");
        assert!(!info.is_live);
        assert!(info.stream_url.is_empty());
        assert!(info.anchor_name.is_empty());
        assert!(info.room_title.is_empty());
        assert!(info.cover_url.is_empty());
        assert!(info.avatar_url.is_empty());
    }

    #[test]
    fn room_finished_response_preserves_saved_details_and_completes_recording() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::database::Database::new(&dir.path().join("rooms.db")).unwrap();
        let id = db
            .add_room_full(
                "douyin",
                "123",
                "测试主播",
                "测试直播间",
                "https://example.com/cover.jpg",
                "https://example.com/avatar.jpg",
                true,
            )
            .unwrap();
        let before = db.get_room(id).unwrap();
        let info = parse_room_response(
            r#"{"data":{"message":"room has finished","prompts":"直播已结束"}}"#,
            "123",
            "ORIGIN",
        )
        .unwrap();
        let updated = crate::apply_live_info_in_db(&db, id, &info).unwrap();

        assert!(!updated.is_live);
        assert_eq!(updated.anchor_name, before.anchor_name);
        assert_eq!(updated.room_title, before.room_title);
        assert_eq!(updated.cover_url, before.cover_url);
        assert_eq!(updated.avatar_url, before.avatar_url);
        let verification = if info.is_live {
            crate::LiveVerification::Live
        } else {
            crate::LiveVerification::Offline
        };
        let exit = crate::recorder::RecordingExit {
            manually_stopped: false,
            status_success: false,
            exit_code: Some(1),
            forced_stop_reason: None,
            wait_error: None,
            stderr_tail: Vec::new(),
        };
        assert_eq!(
            crate::classify_recording(1024, &exit, Some(verification)),
            "completed"
        );
    }

    #[test]
    fn room_finished_message_does_not_depend_on_localized_prompt_or_status_code() {
        for response in [
            json!({"data": {"message": "room has finished"}}),
            json!({"data": {"message": "room has finished", "prompts": "Live has ended"}, "status_code": 0}),
            json!({"data": {"message": "room has finished", "prompts": "直播已结束"}, "status_code": 1}),
        ] {
            let info = parse_room_response(&response.to_string(), "123", "ORIGIN").unwrap();
            assert!(!info.is_live);
            assert!(info.stream_url.is_empty());
        }
    }

    #[test]
    fn room_response_preserves_regular_live_and_offline_details() {
        for status in [2, 4] {
            let response = json!({"data": {"data": [{
                "status": status,
                "title": "测试直播间",
                "owner": {
                    "nickname": "测试主播",
                    "avatar_thumb": {"url_list": ["https://example.com/avatar.jpg"]}
                },
                "cover": {"url_list": ["https://example.com/cover.jpg"]},
                "stream_url": {"flv_pull_url": {
                    "FULL_HD1": "https://example.com/blue.flv",
                    "HD1": "https://example.com/hd.flv"
                }}
            }]}});
            let info = parse_room_response(&response.to_string(), "123", "HD1").unwrap();

            assert_eq!(info.is_live, status == 2);
            assert_eq!(info.anchor_name, "测试主播");
            assert_eq!(info.room_title, "测试直播间");
            assert_eq!(info.avatar_url, "https://example.com/avatar.jpg");
            assert_eq!(info.cover_url, "https://example.com/cover.jpg");
            assert_eq!(info.stream_url, "https://example.com/hd.flv");
        }
    }

    #[test]
    fn room_response_does_not_treat_unknown_errors_or_empty_data_as_offline() {
        for response in [
            json!({}),
            json!({"data": null}),
            json!({"data": {}}),
            json!({"data": {"data": []}}),
            json!({"data": {"data": null}}),
            json!({"data": {"data": [{"status": "invalid"}]}}),
            json!({"data": {"message": "need login", "prompts": "请先登录"}}),
            json!({"data": {"message": "too many requests", "prompts": "请求过于频繁"}}),
            json!({"data": {"message": "room not found"}}),
            json!({"data": {"message": "cannot confirm whether room has finished"}}),
            json!({"data": {"prompts": "直播已结束"}}),
            json!({"data": {}, "message": "room has finished"}),
        ] {
            assert!(
                parse_room_response(&response.to_string(), "123", "ORIGIN").is_err(),
                "unexpected offline result for {response}"
            );
        }
        for body in [
            "",
            "<html>error</html>",
            r#"{"data":{"message":"room has finished"}"#,
        ] {
            assert!(parse_room_response(body, "123", "ORIGIN").is_err());
        }
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
