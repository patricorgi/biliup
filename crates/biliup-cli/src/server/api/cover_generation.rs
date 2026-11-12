use axum::Json;
use axum::body::Bytes;
use axum::extract::Multipart;
use axum::extract::multipart::Field;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use serde_json::json;
use std::sync::LazyLock;
use std::time::Duration;

const MAX_REFERENCES: usize = 8;
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = MAX_REFERENCES * MAX_IMAGE_BYTES + 64 * 1024;
const MODEL: &str = "gpt-image-2.5-flare";
const SIZE: &str = "1536x864";
const QUALITY: &str = "medium";
const COMPRESSION: u8 = 90;
const OPENAI_IMAGES: &str = "https://api.openai.com/v1/images";

static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(240))
        .build()
});

fn configured_key() -> Option<String> {
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub async fn status() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "configured": configured_key().is_some() })),
    )
}

pub async fn generate(headers: HeaderMap, mut body: Multipart) -> Result<Response, Response> {
    // Reject cross-site HTML form posts before processing reference images.
    if headers
        .get("x-biliup-cover")
        .and_then(|value| value.to_str().ok())
        != Some("1")
    {
        return Err((StatusCode::FORBIDDEN, "缺少封面生成请求标记").into_response());
    }
    let mut key = configured_key();
    let mut prompt = None;
    let mut references = Vec::new();
    while let Some(field) = body
        .next_field()
        .await
        .map_err(IntoResponse::into_response)?
    {
        match field.name() {
            Some("api_key") if key.is_none() => key = Some(read_text(field, 1024).await?),
            Some("prompt") if prompt.is_none() => prompt = Some(read_text(field, 16 * 1024).await?),
            Some("reference") if references.len() < MAX_REFERENCES => {
                let bytes = read_field(field, MAX_IMAGE_BYTES, "每张参考图不能超过 5 MB").await?;
                let (extension, mime) = match image::guess_format(&bytes) {
                    Ok(image::ImageFormat::Jpeg) => ("jpg", "image/jpeg"),
                    Ok(image::ImageFormat::Png) => ("png", "image/png"),
                    Ok(image::ImageFormat::WebP) => ("webp", "image/webp"),
                    _ => return Err(bad_request("参考图必须是 JPG、PNG 或 WebP")),
                };
                // Avoid sending local filenames to the image provider.
                let part = Part::bytes(bytes)
                    .file_name(format!("reference-{}.{}", references.len() + 1, extension))
                    .mime_str(mime)
                    .map_err(|_| bad_request("参考图格式无效"))?;
                references.push(part);
            }
            Some("reference") => return Err(bad_request("最多可添加 8 张参考图")),
            _ => return Err(bad_request("封面生成请求包含多余字段")),
        }
    }
    let key = key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| bad_request("请输入 OpenAI API 密钥"))?;
    let prompt = prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| bad_request("请输入封面提示词"))?;
    let client = CLIENT
        .as_ref()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "无法连接 OpenAI").into_response())?;
    let request = if references.is_empty() {
        client
            .post(format!("{OPENAI_IMAGES}/generations"))
            .json(&json!({
                "model": MODEL,
                "prompt": prompt,
                "size": SIZE,
                "quality": QUALITY,
                "output_format": "jpeg",
                "output_compression": COMPRESSION,
            }))
    } else {
        let mut form = Form::new()
            .text("model", MODEL)
            .text("prompt", prompt.to_owned())
            .text("size", SIZE)
            .text("quality", QUALITY)
            .text("output_format", "jpeg")
            .text("output_compression", COMPRESSION.to_string());
        for reference in references {
            form = form.part("image[]", reference);
        }
        client
            .post(format!("{OPENAI_IMAGES}/edits"))
            .multipart(form)
    };
    let mut response = request
        .bearer_auth(key)
        .send()
        .await
        .map_err(upstream_error)?;
    match response.status().as_u16() {
        200..=299 => {}
        401 | 403 => {
            return Err(bad_request(
                "OpenAI 拒绝了请求，请检查 API 密钥和图片生成权限",
            ));
        }
        402 | 429 => {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "OpenAI 额度不足或请求过于频繁",
            )
                .into_response());
        }
        400 | 422 => return Err(bad_request("OpenAI 无法处理该描述或参考图，请调整后重试")),
        _ => return Err((StatusCode::BAD_GATEWAY, "OpenAI 图片服务暂时不可用").into_response()),
    }

    // Limit the encoded response before allocating or decoding the image.
    let mut encoded = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(upstream_error)? {
        if encoded.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err(invalid_result());
        }
        encoded.extend_from_slice(&chunk);
    }
    let result: ImagesResponse = serde_json::from_slice(&encoded).map_err(|_| invalid_result())?;
    let image = result.data.first().ok_or_else(invalid_result)?;
    let bytes = STANDARD
        .decode(&image.b64_json)
        .map_err(|_| invalid_result())?;
    let jpeg = tokio::task::spawn_blocking(move || super::endpoints::cover_jpeg(bytes))
        .await
        .map_err(|_| invalid_result())?
        .map_err(|_| invalid_result())?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Bytes::from(jpeg),
    )
        .into_response())
}

#[derive(Deserialize)]
struct ImagesResponse {
    data: Vec<GeneratedImage>,
}

#[derive(Deserialize)]
struct GeneratedImage {
    b64_json: String,
}

async fn read_text(field: Field<'_>, limit: usize) -> Result<String, Response> {
    let bytes = read_field(field, limit, "封面提示词或密钥过长").await?;
    String::from_utf8(bytes).map_err(|_| bad_request("封面生成请求包含无效文字"))
}

async fn read_field(
    mut field: Field<'_>,
    limit: usize,
    message: &'static str,
) -> Result<Vec<u8>, Response> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(IntoResponse::into_response)? {
        if bytes.len() + chunk.len() > limit {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, message).into_response());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn bad_request(message: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, message).into_response()
}

fn upstream_error(error: reqwest::Error) -> Response {
    if error.is_timeout() {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            "等待 OpenAI 生成封面超时，请稍后重试",
        )
            .into_response();
    }
    (StatusCode::BAD_GATEWAY, "无法连接 OpenAI 图片服务").into_response()
}

fn invalid_result() -> Response {
    (StatusCode::BAD_GATEWAY, "OpenAI 返回的封面无效或超过 5 MB").into_response()
}
