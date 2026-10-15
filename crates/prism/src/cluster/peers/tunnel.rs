//! HTTP forwarding: a request for another node's resource is tunnelled to
//! that node over gRPC and served by its own HTTP API.

use std::sync::Arc;

use axum::body::Body;
use futures::StreamExt;
use prism_proto::v1::http_tunnel_client::HttpTunnelClient;
use prism_proto::v1::http_tunnel_server::HttpTunnel;
use prism_proto::v1::{HttpFrame, HttpHeader};
use tonic::{Request, Response, Status, Streaming};

use super::{BoxStream, grpc_addr};
use crate::cluster::model::MemberDoc;
use crate::cluster::runtime::Cluster;

/// Marks a request that arrived through the tunnel from a peer: it skips
/// request hygiene (already checked by the first node) and is never
/// forwarded again.
#[derive(Debug, Clone, Copy)]
pub struct Tunnelled;

pub const FORWARDED_BY: &str = "x-prism-forwarded-by";
pub const SERVED_BY: &str = "x-prism-served-by";
const FRAME_CHUNK: usize = 256 * 1024;

pub struct TunnelService {
    pub cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl HttpTunnel for TunnelService {
    type ForwardStream = BoxStream<HttpFrame>;

    /// Runs a forwarded request through this node's own HTTP API.
    async fn forward(
        &self,
        req: Request<Streaming<HttpFrame>>,
    ) -> Result<Response<Self::ForwardStream>, Status> {
        crate::cluster::grpc::check_active(&self.cluster, req.remote_addr())?;
        let router = self
            .cluster
            .http
            .get()
            .cloned()
            .ok_or_else(|| Status::unavailable("node is starting"))?;
        let me = self.cluster.me;
        let mut frames = req.into_inner();
        let head = frames
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("missing request head"))?;
        // Body: the head's bytes, then frames until one says `end`.
        let first = bytes::Bytes::from(head.body.clone());
        let rest = if head.end {
            futures::stream::empty().boxed()
        } else {
            futures::stream::unfold(frames, |mut s| async move {
                match s.message().await {
                    Ok(Some(f)) if f.end && f.body.is_empty() => None,
                    Ok(Some(f)) => Some((Ok(bytes::Bytes::from(f.body)), s)),
                    Ok(None) => None,
                    Err(e) => Some((Err(std::io::Error::other(e.to_string())), s)),
                }
            })
            .boxed()
        };
        let body_stream = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
            .chain(rest)
            .filter(|b| futures::future::ready(b.as_ref().map_or(true, |b| !b.is_empty())));
        let mut builder = axum::http::Request::builder()
            .method(head.method.as_str())
            .uri(&head.path);
        for h in &head.headers {
            builder = builder.header(&h.name, h.value.as_slice());
        }
        let mut request = builder
            .body(Body::from_stream(body_stream))
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        request.extensions_mut().insert(Tunnelled);
        let response = tower::ServiceExt::oneshot(router, request)
            .await
            .unwrap_or_else(|e| match e {});
        let (parts, body) = response.into_parts();
        let mut headers: Vec<HttpHeader> = parts
            .headers
            .iter()
            .map(|(k, v)| HttpHeader {
                name: k.as_str().to_string(),
                value: v.as_bytes().to_vec(),
            })
            .collect();
        headers.push(HttpHeader {
            name: SERVED_BY.into(),
            value: me.to_string().into_bytes(),
        });
        let head = HttpFrame {
            status: u32::from(parts.status.as_u16()),
            headers,
            ..HttpFrame::default()
        };
        let body = body.into_data_stream().map(|chunk| match chunk {
            Ok(b) => Ok(HttpFrame {
                body: b.to_vec(),
                ..HttpFrame::default()
            }),
            Err(e) => Err(Status::internal(e.to_string())),
        });
        let out = futures::stream::once(async move { Ok(head) })
            .chain(body)
            .chain(futures::stream::once(async {
                Ok(HttpFrame {
                    end: true,
                    ..HttpFrame::default()
                })
            }));
        Ok(Response::new(Box::pin(out)))
    }
}

/// Forwards an HTTP request to `member` and streams the response back.
pub async fn forward_http(
    cluster: &Cluster,
    member: &MemberDoc,
    req: axum::http::Request<Body>,
) -> anyhow::Result<axum::response::Response> {
    let (parts, body) = req.into_parts();
    let mut headers: Vec<HttpHeader> = parts
        .headers
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "connection" | "transfer-encoding" | "keep-alive" | "upgrade"
            )
        })
        .map(|(k, v)| HttpHeader {
            name: k.as_str().to_string(),
            value: v.as_bytes().to_vec(),
        })
        .collect();
    headers.push(HttpHeader {
        name: FORWARDED_BY.into(),
        value: cluster.me.to_string().into_bytes(),
    });
    let head = HttpFrame {
        method: parts.method.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".into(), ToString::to_string),
        headers,
        ..HttpFrame::default()
    };
    let body = body.into_data_stream().flat_map(|chunk| {
        let frames: Vec<HttpFrame> = match chunk {
            Ok(b) => b
                .chunks(FRAME_CHUNK)
                .map(|c| HttpFrame {
                    body: c.to_vec(),
                    ..HttpFrame::default()
                })
                .collect(),
            Err(_) => vec![],
        };
        futures::stream::iter(frames)
    });
    let outbound = futures::stream::once(async move { head })
        .chain(body)
        .chain(futures::stream::once(async {
            HttpFrame {
                end: true,
                ..HttpFrame::default()
            }
        }));
    let ch = cluster.channels.get(&grpc_addr(member))?;
    let mut inbound = HttpTunnelClient::new(ch)
        .forward(outbound)
        .await?
        .into_inner();
    let head = inbound
        .message()
        .await?
        .ok_or_else(|| anyhow::anyhow!("peer closed the tunnel"))?;
    let mut res = axum::http::Response::builder().status(u16::try_from(head.status)?);
    for h in &head.headers {
        res = res.header(&h.name, h.value.as_slice());
    }
    let body = futures::stream::unfold(inbound, |mut s| async move {
        match s.message().await {
            Ok(Some(f)) if f.end => None,
            Ok(Some(f)) => Some((Ok::<_, std::io::Error>(bytes::Bytes::from(f.body)), s)),
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e.to_string())), s)),
        }
    });
    Ok(res.body(Body::from_stream(body))?)
}
