use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde::Deserialize;

use crate::settings::AppSettings;

const DOUYIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

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
    id_str: Option<String>,
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

fn extract_room_id(url: &str) -> Result<String, String> {
    let url = url.trim();

    if url.contains("live.douyin.com") {
        let path = url
            .split("live.douyin.com")
            .nth(1)
            .ok_or("无法解析直播间链接")?;
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
        return Err("暂不支持短链接，请复制完整的直播间链接 (live.douyin.com/...)".to_string());
    }

    Err("无法解析直播间ID，请输入 live.douyin.com 格式的链接".to_string())
}

fn build_client(settings: &AppSettings) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();

    if !settings.proxy.is_empty() {
        let proxy = reqwest::Proxy::all(&settings.proxy)
            .map_err(|e| format!("代理配置无效: {}", e))?;
        builder = builder.proxy(proxy);
    }

    builder.build().map_err(|e| format!("创建 HTTP 客户端失败: {}", e))
}

async fn get_ttwid(client: &reqwest::Client) -> Result<String, String> {
    let resp = client
        .get("https://live.douyin.com")
        .header(USER_AGENT, DOUYIN_UA)
        .send()
        .await
        .map_err(|e| format!("获取 ttwid 失败: {}", e))?;

    let cookies = resp.headers().get_all("set-cookie");
    for cookie in cookies {
        let cookie_str = cookie.to_str().unwrap_or("");
        if cookie_str.starts_with("ttwid=") {
            let ttwid = cookie_str
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

    Err("无法获取 ttwid cookie，抖音可能更新了接口".to_string())
}

pub async fn parse_douyin_url(url: &str, settings: &AppSettings) -> Result<LiveInfo, String> {
    let room_id = extract_room_id(url)?;
    let client = build_client(settings)?;

    // Get ttwid (use proxy if configured)
    let ttwid = get_ttwid(&client).await?;

    // Build cookie: ttwid + user-provided cookie
    let mut cookie_value = format!("ttwid={}", ttwid);
    if !settings.cookie.is_empty() {
        cookie_value = format!("{}; {}", cookie_value, settings.cookie);
    }

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(DOUYIN_UA));
    headers.insert(REFERER, HeaderValue::from_static("https://live.douyin.com/"));
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&cookie_value)
            .map_err(|_| "构建 cookie 失败，可能包含非法字符".to_string())?,
    );

    let api_url = format!(
        "https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web\
        &live_id=1&device_platform=web&language=zh-CN&enter_from=web_live\
        &cookie_enabled=true&browser_language=zh-CN&browser_platform=Win32\
        &browser_name=Chrome&browser_version=120&web_rid={}",
        room_id
    );

    let resp = client
        .get(&api_url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("请求抖音 API 失败: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("抖音 API 返回错误: HTTP {}", status));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;

    let preview: String = body.chars().take(100).collect();
    let api_resp: ApiResponse = serde_json::from_str(&body)
        .map_err(|e| format!("解析 API 响应失败: {} (响应: {})", e, preview))?;

    let room = api_resp
        .data
        .data
        .into_iter()
        .next()
        .ok_or("API 返回的房间数据为空，直播间可能不存在")?;

    let is_live = room.status == Some(2);

    let anchor_name = room
        .owner
        .as_ref()
        .and_then(|o| o.nickname.clone())
        .unwrap_or_default();

    let room_title = room.title.unwrap_or_default();

    let cover_url = room
        .cover
        .as_ref()
        .and_then(|c| c.url_list.as_ref())
        .and_then(|urls| urls.first().cloned())
        .unwrap_or_default();

    let avatar_url = room
        .owner
        .as_ref()
        .and_then(|o| o.avatar_thumb.as_ref())
        .and_then(|a| a.url_list.as_ref())
        .and_then(|urls| urls.first().cloned())
        .unwrap_or_default();

    let preferred_quality = if settings.quality.is_empty() { "HD1" } else { &settings.quality };
    let stream_url = if let Some(ref stream_url) = room.stream_url {
        get_best_stream_url(stream_url, preferred_quality)
    } else {
        String::new()
    };

    Ok(LiveInfo {
        platform: "douyin".to_string(),
        room_id,
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

    if let Some(ref flv) = stream_url.flv_pull_url {
        if let Some(obj) = flv.as_object() {
            for quality in &fallback_order {
                if let Some(url) = obj.get(*quality).and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        return url.to_string();
                    }
                }
            }
        }
    }

    if let Some(ref hls) = stream_url.hls_pull_url_map {
        if let Some(obj) = hls.as_object() {
            for quality in &fallback_order {
                if let Some(url) = obj.get(*quality).and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        return url.to_string();
                    }
                }
            }
        }
    }

    String::new()
}
