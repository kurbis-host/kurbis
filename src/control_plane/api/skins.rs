//! A Java player's skin, by name, for the landing page: a visitor whose server name is
//! also their Minecraft name sees themselves in the film. Mojang's services do not answer
//! browsers, so the control plane asks for them, and keeps every answer (a skin, or that
//! there is none) for a day. A cap on lookups a minute keeps a page that is hammered from
//! spending the host's allowance with Mojang.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use base64::Engine as _;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    ApiError, ApiPath, AppState,
    player_resolution::{MOJANG_PROFILE_BASE_URL, is_java_profile_name, resolve_java_profile},
};

const SESSION_PROFILE_BASE_URL: &str = "https://sessionserver.mojang.com/session/minecraft/profile";
/// The only host a skin is fetched from, whatever a profile says.
const TEXTURES_HOST: &str = "textures.minecraft.net";
const MAX_SESSION_PROFILE_BYTES: usize = 16 * 1024;
const MAX_SKIN_BYTES: usize = 64 * 1024;
/// How long an answer is kept, and how many names may be looked up a minute in all.
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);
const LOOKUPS_A_MINUTE: usize = 60;
/// Answers kept at most; the oldest go first.
const KEPT: usize = 4096;
pub(super) const SKIN_MODEL_HEADER: &str = "x-skin-model";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SkinPath {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Model {
    Classic,
    Slim,
}

#[derive(Debug, Clone)]
pub(super) struct Skin {
    pub(super) png: Arc<Vec<u8>>,
    pub(super) model: Model,
}

/// Where the lookups go: Mojang, or a stand-in in tests.
#[derive(Debug, Clone)]
pub(super) struct Sources {
    pub(super) profiles: String,
    pub(super) sessions: String,
    /// Skins are fetched over https from `TEXTURES_HOST` unless this says where else (tests only).
    pub(super) textures: Option<String>,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            profiles: MOJANG_PROFILE_BASE_URL.to_owned(),
            sessions: SESSION_PROFILE_BASE_URL.to_owned(),
            textures: None,
        }
    }
}

struct Kept {
    answers: HashMap<String, (Instant, Option<Skin>)>,
    lookups: Vec<Instant>,
}

static KEPT_ANSWERS: Mutex<Option<Kept>> = Mutex::new(None);

pub(super) async fn player_skin(
    State(state): State<Arc<AppState>>,
    ApiPath(path): ApiPath<SkinPath>,
) -> Result<Response, ApiError> {
    match skin_for(&state.http, &Sources::default(), &path.name).await? {
        Some(skin) => Ok(png_response(skin)),
        None => Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "skin_not_found",
            "no Java player with a skin of their own has that name",
        )),
    }
}

fn png_response(skin: Skin) -> Response {
    let mut response = Response::new(Body::from(skin.png.as_ref().clone()));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    headers.insert(
        SKIN_MODEL_HEADER,
        HeaderValue::from_static(match skin.model {
            Model::Classic => "classic",
            Model::Slim => "slim",
        }),
    );
    response
}

/// The skin of the Java player with this name, if there is one with a skin of their own.
pub(super) async fn skin_for(
    http: &reqwest::Client,
    sources: &Sources,
    name: &str,
) -> Result<Option<Skin>, ApiError> {
    // A name no Java account can have is not asked about.
    if !is_java_profile_name(name) {
        return Ok(None);
    }
    let key = name.to_ascii_lowercase();
    {
        let mut kept = KEPT_ANSWERS.lock().expect("skin cache lock");
        let kept = kept.get_or_insert_with(|| Kept {
            answers: HashMap::new(),
            lookups: Vec::new(),
        });
        let now = Instant::now();
        if let Some((at, answer)) = kept.answers.get(&key)
            && now.duration_since(*at) < KEEP
        {
            return Ok(answer.clone());
        }
        kept.lookups
            .retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        if kept.lookups.len() >= LOOKUPS_A_MINUTE {
            return Err(ApiError::with_code(
                StatusCode::TOO_MANY_REQUESTS,
                "skin_lookup_rate_limited",
                "too many skins were looked up just now; try again in a minute",
            ));
        }
        kept.lookups.push(now);
    }
    let answer = match look_up(http, sources, name).await {
        Ok(answer) => answer,
        // No such player is an answer, kept like any other.
        Err(error) if error.code == "minecraft_player_not_found" => None,
        Err(error) => return Err(error),
    };
    let mut kept = KEPT_ANSWERS.lock().expect("skin cache lock");
    if let Some(kept) = kept.as_mut() {
        if kept.answers.len() >= KEPT {
            let now = Instant::now();
            kept.answers
                .retain(|_, (at, _)| now.duration_since(*at) < KEEP);
            if kept.answers.len() >= KEPT {
                kept.answers.clear();
            }
        }
        kept.answers.insert(key, (Instant::now(), answer.clone()));
    }
    Ok(answer)
}

#[derive(Debug, Deserialize)]
struct SessionProfile {
    #[serde(default)]
    properties: Vec<SessionProperty>,
}

#[derive(Debug, Deserialize)]
struct SessionProperty {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct Textures {
    textures: TextureSet,
}

#[derive(Debug, Deserialize)]
struct TextureSet {
    #[serde(rename = "SKIN")]
    skin: Option<TextureEntry>,
}

#[derive(Debug, Deserialize)]
struct TextureEntry {
    url: String,
    metadata: Option<TextureMetadata>,
}

#[derive(Debug, Deserialize)]
struct TextureMetadata {
    model: Option<String>,
}

async fn look_up(
    http: &reqwest::Client,
    sources: &Sources,
    name: &str,
) -> Result<Option<Skin>, ApiError> {
    let player = resolve_java_profile(http, &sources.profiles, name).await?;
    let profile = fetch(
        http,
        &format!(
            "{}/{}",
            sources.sessions.trim_end_matches('/'),
            player.player_id.simple()
        ),
        MAX_SESSION_PROFILE_BYTES,
    )
    .await?;
    let profile: SessionProfile = serde_json::from_slice(&profile).map_err(|_| unavailable())?;
    let Some(textures) = profile
        .properties
        .iter()
        .find(|property| property.name == "textures")
    else {
        return Ok(None);
    };
    let textures = base64::engine::general_purpose::STANDARD
        .decode(textures.value.as_bytes())
        .map_err(|_| unavailable())?;
    let textures: Textures = serde_json::from_slice(&textures).map_err(|_| unavailable())?;
    // A player who never set a skin wears one of the defaults: nothing of their own to show.
    let Some(skin) = textures.textures.skin else {
        return Ok(None);
    };
    let url = skin_url(&skin.url, sources.textures.as_deref()).ok_or_else(unavailable)?;
    let png = fetch(http, &url, MAX_SKIN_BYTES).await?;
    if !is_skin_png(&png) {
        return Err(unavailable());
    }
    let model = match skin.metadata.and_then(|metadata| metadata.model).as_deref() {
        Some("slim") => Model::Slim,
        _ => Model::Classic,
    };
    tracing::debug!(player = %player.name, id = %player.player_id, "skin looked up");
    Ok(Some(Skin {
        png: Arc::new(png),
        model,
    }))
}

/// Where to fetch a skin from: only ever `TEXTURES_HOST`, over https.
fn skin_url(url: &str, textures: Option<&str>) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if parsed.host_str() != Some(TEXTURES_HOST) || !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let path = parsed.path();
    if !path.starts_with("/texture/") || path.len() > 160 {
        return None;
    }
    Some(match textures {
        Some(base) => format!("{}{path}", base.trim_end_matches('/')),
        None => format!("https://{TEXTURES_HOST}{path}"),
    })
}

/// A PNG of a skin's size: 64 wide, and 64 tall (or 32, the old layout).
fn is_skin_png(bytes: &[u8]) -> bool {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || !bytes.starts_with(SIGNATURE) || &bytes[12..16] != b"IHDR" {
        return false;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    width == 64 && (height == 64 || height == 32)
}

async fn fetch(http: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, ApiError> {
    let mut response = http.get(url).send().await.map_err(|error| {
        tracing::warn!(?error, "skin lookup failed");
        unavailable()
    })?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ApiError::with_code(
            StatusCode::TOO_MANY_REQUESTS,
            "skin_lookup_rate_limited",
            "Minecraft's skin service is busy; try again later",
        ));
    }
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > limit as u64)
    {
        return Err(unavailable());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if body.len() + chunk.len() > limit {
            return Err(unavailable());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn unavailable() -> ApiError {
    ApiError::with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        "skin_lookup_unavailable",
        "Minecraft's skin service is unavailable just now",
    )
}

#[cfg(test)]
pub(super) fn forget_kept() {
    *KEPT_ANSWERS.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// A stand-in for Mojang: the profile, the session profile and the skin, each at its path.
    async fn mojang(
        profile: Option<&'static str>,
        textures: Option<String>,
        png: Vec<u8>,
    ) -> (Sources, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0; 2048];
                let read = stream.read(&mut request).await.unwrap_or(0);
                let line = String::from_utf8_lossy(&request[..read])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned();
                let path = line.split(' ').nth(1).unwrap_or("").to_owned();
                seen.lock().unwrap().push(path.clone());
                let (status, kind, body): (u16, &str, Vec<u8>) =
                    if path.starts_with("/users/profiles/minecraft/") {
                        match profile {
                            Some(body) => (200, "application/json", body.as_bytes().to_vec()),
                            None => (404, "application/json", b"{}".to_vec()),
                        }
                    } else if path.starts_with("/session/") {
                        let properties = textures
                            .as_ref()
                            .map(|value| format!(r#"[{{"name":"textures","value":"{value}"}}]"#))
                            .unwrap_or_else(|| "[]".to_owned());
                        (
                            200,
                            "application/json",
                            format!(r#"{{"id":"x","name":"x","properties":{properties}}}"#)
                                .into_bytes(),
                        )
                    } else if path.starts_with("/texture/") {
                        (200, "image/png", png.clone())
                    } else {
                        (404, "text/plain", Vec::new())
                    };
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            }
        });
        let base = format!("http://{address}");
        (
            Sources {
                profiles: format!("{base}/users/profiles/minecraft"),
                sessions: format!("{base}/session/minecraft/profile"),
                textures: Some(base),
            },
            asked,
        )
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn textures(model: Option<&str>) -> String {
        let metadata = model
            .map(|model| format!(r#","metadata":{{"model":"{model}"}}"#))
            .unwrap_or_default();
        base64::engine::general_purpose::STANDARD.encode(format!(
            r#"{{"textures":{{"SKIN":{{"url":"http://textures.minecraft.net/texture/abc123"{metadata}}}}}}}"#
        ))
    }

    const NOTCH: &str = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch"}"#;

    // (One test, in order: the answers are kept for the whole process.)
    #[tokio::test]
    async fn looks_up_a_skin_keeps_the_answer_and_refuses_what_it_cannot_be() {
        forget_kept();
        let http = reqwest::Client::new();

        // A player with a slim skin of their own: its PNG and its model, then the same from memory.
        let (sources, asked) = mojang(Some(NOTCH), Some(textures(Some("slim"))), png(64, 64)).await;
        let skin = skin_for(&http, &sources, "Notch").await.unwrap().unwrap();
        assert_eq!(skin.model, Model::Slim);
        assert_eq!(&skin.png[..8], b"\x89PNG\r\n\x1a\n");
        let first = asked.lock().unwrap().len();
        assert_eq!(first, 3, "profile, session profile, skin");
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == "/session/minecraft/profile/069a79f444e94726a5befca90e38aaf5")
        );
        let again = skin_for(&http, &sources, "NOTCH").await.unwrap().unwrap();
        assert_eq!(again.model, Model::Slim);
        assert_eq!(asked.lock().unwrap().len(), first, "kept, not asked again");

        // No such player: no skin, and that is kept too.
        let (sources, asked) = mojang(None, None, Vec::new()).await;
        assert!(
            skin_for(&http, &sources, "nobodyhere")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            skin_for(&http, &sources, "nobodyhere")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(asked.lock().unwrap().len(), 1);

        // A player wearing a default skin has none of their own.
        let (sources, _) = mojang(
            Some(r#"{"id":"069a79f444e94726a5befca90e38aaf6","name":"plainone"}"#),
            None,
            Vec::new(),
        )
        .await;
        assert!(
            skin_for(&http, &sources, "plainone")
                .await
                .unwrap()
                .is_none()
        );

        // A classic skin, in the old 64 by 32 layout.
        let (sources, _) = mojang(
            Some(r#"{"id":"069a79f444e94726a5befca90e38aaf7","name":"oldone"}"#),
            Some(textures(None)),
            png(64, 32),
        )
        .await;
        assert_eq!(
            skin_for(&http, &sources, "oldone")
                .await
                .unwrap()
                .unwrap()
                .model,
            Model::Classic
        );

        // Something that is not a skin is refused, and nothing is kept for it.
        let (sources, _) = mojang(
            Some(r#"{"id":"069a79f444e94726a5befca90e38aaf8","name":"bigone"}"#),
            Some(textures(None)),
            png(512, 512),
        )
        .await;
        assert_eq!(
            skin_for(&http, &sources, "bigone").await.unwrap_err().code,
            "skin_lookup_unavailable"
        );

        // Names no Java account can have are never asked about.
        let (sources, asked) = mojang(Some(NOTCH), None, Vec::new()).await;
        for name in ["my-world", "ab", "a-very-long-server-name-indeed"] {
            assert!(skin_for(&http, &sources, name).await.unwrap().is_none());
        }
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn skins_come_only_from_minecrafts_texture_host() {
        assert_eq!(
            skin_url("http://textures.minecraft.net/texture/abc123", None).as_deref(),
            Some("https://textures.minecraft.net/texture/abc123")
        );
        for url in [
            "http://evil.example/texture/abc",
            "http://textures.minecraft.net.evil.example/texture/abc",
            "http://textures.minecraft.net/other/abc",
            "file:///texture/abc",
            "not a url",
        ] {
            assert_eq!(skin_url(url, None), None, "{url}");
        }
    }

    #[test]
    fn only_a_skin_sized_png_passes() {
        assert!(is_skin_png(&png(64, 64)));
        assert!(is_skin_png(&png(64, 32)));
        assert!(!is_skin_png(&png(128, 128)));
        assert!(!is_skin_png(b"GIF89a not a png at all"));
    }
}
