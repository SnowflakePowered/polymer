use std::{net::SocketAddr, ops::Range, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{
        HeaderMap, Method, Response, StatusCode,
        header::{
            ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL, CONTENT_LENGTH,
            CONTENT_RANGE, CONTENT_TYPE, RANGE,
        },
    },
    routing::get,
};
use object_store::ObjectStoreExt;
use polymer_library::{GameCatalog, MediaData};
use uuid::Uuid;

pub(super) struct MediaServer {
    address: SocketAddr,
    token: Arc<str>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct ServerState {
    catalog: Arc<GameCatalog>,
    token: Arc<str>,
}

impl MediaServer {
    pub(super) async fn start(catalog: Arc<GameCatalog>) -> Result<Self, std::io::Error> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let token: Arc<str> = format!(
            "{:016x}{:016x}{:016x}{:016x}",
            rand::random::<u64>(),
            rand::random::<u64>(),
            rand::random::<u64>(),
            rand::random::<u64>()
        )
        .into();
        let router = Router::new()
            .route(
                "/library/media/{token}/{metadata_id}/{hash}",
                get(Self::serve),
            )
            .with_state(ServerState {
                catalog,
                token: token.clone(),
            });
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(%error, "library media server stopped");
            }
        });
        Ok(Self {
            address,
            token,
            task,
        })
    }

    pub(super) fn url(&self, metadata_id: Uuid, hash: &str) -> String {
        format!(
            "http://{}/library/media/{}/{}/{}",
            self.address,
            self.token,
            metadata_id.simple(),
            hash
        )
    }

    async fn serve(
        State(state): State<ServerState>,
        Path((token, metadata_id, hash)): Path<(String, String, String)>,
        method: Method,
        headers: HeaderMap,
    ) -> Response<Body> {
        let Some(id) = Uuid::parse_str(&metadata_id)
            .ok()
            .filter(|_| token == state.token.as_ref())
        else {
            return Self::status(StatusCode::NOT_FOUND);
        };
        let Ok(resource) = state.catalog.media(id).await else {
            return Self::status(StatusCode::NOT_FOUND);
        };
        let expected = blake3::Hash::from_bytes(resource.content_blake3)
            .to_hex()
            .to_string();
        let MediaData::Object(location) = resource.data else {
            return Self::status(StatusCode::NOT_FOUND);
        };
        if hash != expected {
            return Self::status(StatusCode::NOT_FOUND);
        }
        let range = match headers.get(RANGE) {
            Some(value) => match value
                .to_str()
                .ok()
                .and_then(|value| Self::range(value, resource.byte_length))
            {
                Some(range) => Some(range),
                None => return Self::range_error(resource.byte_length),
            },
            None => None,
        };
        let bytes = if method == Method::HEAD {
            None
        } else {
            let result = match &range {
                Some(range) => {
                    state
                        .catalog
                        .objects()
                        .get_range(&location, range.clone())
                        .await
                }
                None => match state.catalog.objects().get(&location).await {
                    Ok(result) => result.bytes().await,
                    Err(error) => Err(error),
                },
            };
            match result {
                Ok(bytes) => Some(bytes),
                Err(_) => return Self::status(StatusCode::NOT_FOUND),
            }
        };
        let response_length = range
            .as_ref()
            .map_or(resource.byte_length, |range| range.end - range.start);
        let mut response = Response::new(bytes.map_or_else(Body::empty, Body::from));
        *response.status_mut() = if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        let response_headers = response.headers_mut();
        response_headers.insert(
            CONTENT_TYPE,
            resource.media_type.parse().expect("validated MIME type"),
        );
        response_headers.insert(CONTENT_LENGTH, response_length.into());
        response_headers.insert(ACCEPT_RANGES, "bytes".parse().expect("static header"));
        response_headers.insert(
            ACCESS_CONTROL_ALLOW_ORIGIN,
            "*".parse().expect("static header"),
        );
        response_headers.insert(
            CACHE_CONTROL,
            "private, no-store".parse().expect("static header"),
        );
        if let Some(range) = range {
            response_headers.insert(
                CONTENT_RANGE,
                format!(
                    "bytes {}-{}/{}",
                    range.start,
                    range.end - 1,
                    resource.byte_length
                )
                .parse()
                .expect("numeric content range"),
            );
        }
        response
    }

    fn range(value: &str, length: u64) -> Option<Range<u64>> {
        let value = value.strip_prefix("bytes=")?;
        if value.contains(',') || length == 0 {
            return None;
        }
        let (start, end) = value.split_once('-')?;
        if start.is_empty() {
            let suffix = end.parse::<u64>().ok()?.min(length);
            return (suffix > 0).then_some(length - suffix..length);
        }
        let start = start.parse::<u64>().ok()?;
        let end = if end.is_empty() {
            length
        } else {
            end.parse::<u64>().ok()?.checked_add(1)?.min(length)
        };
        (start < end && start < length).then_some(start..end)
    }

    fn status(status: StatusCode) -> Response<Body> {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status;
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_ORIGIN,
            "*".parse().expect("static header"),
        );
        response
    }

    fn range_error(length: u64) -> Response<Body> {
        let mut response = Self::status(StatusCode::RANGE_NOT_SATISFIABLE);
        response.headers_mut().insert(
            CONTENT_RANGE,
            format!("bytes */{length}")
                .parse()
                .expect("numeric content range"),
        );
        response
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
