//! Client tests against a fake daemon on a socket pair.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::*;
use crate::events::DomainEvent;
use crate::proto::{DomainState, LifecycleEvent};
use crate::xdr::Decoder;

/// The daemon side of the socket pair.
struct Daemon(UnixStream);

impl Daemon {
    async fn read_frame(&mut self) -> (Header, Vec<u8>) {
        let len = self.0.read_u32().await.unwrap() as usize;
        let mut buf = vec![0; len - 4];
        self.0.read_exact(&mut buf).await.unwrap();
        let h = Header::decode(&mut Decoder::new(&buf)).unwrap();
        (h, buf[proto::HEADER_LEN..].to_vec())
    }

    async fn write(&mut self, h: &Header, body: &[u8]) {
        self.0.write_all(&frame(h, body).unwrap()).await.unwrap();
    }

    async fn reply(&mut self, call: &Header, status: Status, body: &[u8]) {
        let h = Header {
            ty: MessageType::Reply,
            status,
            ..*call
        };
        self.write(&h, body).await;
    }

    async fn stream_msg(&mut self, call: &Header, status: Status, body: &[u8]) {
        let h = Header {
            ty: MessageType::Stream,
            status,
            ..*call
        };
        self.write(&h, body).await;
    }
}

fn pair() -> (Client, Daemon) {
    let (a, b) = UnixStream::pair().unwrap();
    (Client::from_stream(a), Daemon(b))
}

fn error_body(code: i32, message: &str) -> Vec<u8> {
    let mut e = Encoder::new();
    e.i32(code).i32(0).opt_string(Some(message)).i32(2);
    e.into_inner()
}

fn domain() -> Domain {
    Domain {
        name: "vm".into(),
        uuid: [7; 16],
        id: 3,
    }
}

#[tokio::test]
async fn call_returns_reply_body() {
    let (client, mut daemon) = pair();
    let call = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .call(procs::CONNECT_GET_HOSTNAME, vec![1, 2, 3, 4])
                .await
        }
    });
    let (h, body) = daemon.read_frame().await;
    assert_eq!(h.proc, procs::CONNECT_GET_HOSTNAME);
    assert_eq!(h.ty, MessageType::Call);
    assert_eq!(body, [1, 2, 3, 4]);
    daemon.reply(&h, Status::Ok, &[9, 9, 9, 9]).await;
    assert_eq!(call.await.unwrap().unwrap(), [9, 9, 9, 9]);
}

#[tokio::test]
async fn concurrent_calls_match_replies_by_serial() {
    let (client, mut daemon) = pair();
    let first = tokio::spawn({
        let c = client.clone();
        async move { c.call(1, vec![0, 0, 0, 1]).await }
    });
    let (h1, _) = daemon.read_frame().await;
    let second = tokio::spawn({
        let c = client.clone();
        async move { c.call(2, vec![0, 0, 0, 2]).await }
    });
    let (h2, _) = daemon.read_frame().await;
    assert_ne!(h1.serial, h2.serial);
    daemon.reply(&h2, Status::Ok, &[0, 0, 0, 22]).await;
    daemon.reply(&h1, Status::Ok, &[0, 0, 0, 11]).await;
    assert_eq!(second.await.unwrap().unwrap(), [0, 0, 0, 22]);
    assert_eq!(first.await.unwrap().unwrap(), [0, 0, 0, 11]);
}

#[tokio::test]
async fn error_reply_becomes_remote_error() {
    let (client, mut daemon) = pair();
    let call = tokio::spawn({
        let c = client.clone();
        async move { c.call(1, Vec::new()).await }
    });
    let (h, _) = daemon.read_frame().await;
    daemon
        .reply(&h, Status::Error, &error_body(42, "no such domain"))
        .await;
    let err = call.await.unwrap().unwrap_err();
    assert_eq!(err.remote_code(), Some(42));
    assert!(err.to_string().contains("no such domain"));
}

#[tokio::test]
async fn typed_procedure_decodes_reply() {
    let (client, mut daemon) = pair();
    let dom = domain();
    let call = tokio::spawn({
        let c = client.clone();
        let dom = dom.clone();
        async move { c.get_state(&dom).await }
    });
    let (h, _) = daemon.read_frame().await;
    assert_eq!(h.proc, procs::DOMAIN_GET_STATE);
    let mut e = Encoder::new();
    e.i32(1).i32(5);
    daemon.reply(&h, Status::Ok, &e.into_inner()).await;
    let state = call.await.unwrap().unwrap();
    assert_eq!(state.state, DomainState::Running);
    assert_eq!(state.reason, 5);
}

#[tokio::test]
async fn events_reach_subscribers() {
    let (client, mut daemon) = pair();
    let mut events = client.events();
    let mut e = Encoder::new();
    e.i32(1);
    domain().encode(&mut e);
    e.i32(2).i32(0);
    let h = Header {
        prog: proto::REMOTE_PROGRAM,
        vers: proto::REMOTE_PROTOCOL_VERSION,
        proc: procs::DOMAIN_EVENT_CALLBACK_LIFECYCLE,
        ty: MessageType::Message,
        serial: 0,
        status: Status::Ok,
    };
    daemon.write(&h, &e.into_inner()).await;
    let ev = events.recv().await.unwrap();
    assert_eq!(
        ev,
        DomainEvent::Lifecycle {
            domain: domain(),
            event: LifecycleEvent::Started,
            detail: 0,
        }
    );
}

#[tokio::test]
async fn undecodable_event_does_not_break_the_connection() {
    let (client, mut daemon) = pair();
    let bad = Header {
        prog: proto::REMOTE_PROGRAM,
        vers: proto::REMOTE_PROTOCOL_VERSION,
        proc: procs::DOMAIN_EVENT_CALLBACK_LIFECYCLE,
        ty: MessageType::Message,
        serial: 0,
        status: Status::Ok,
    };
    daemon.write(&bad, &[1]).await;
    let call = tokio::spawn({
        let c = client.clone();
        async move { c.call(1, Vec::new()).await }
    });
    let (h, _) = daemon.read_frame().await;
    daemon.reply(&h, Status::Ok, &[]).await;
    assert!(call.await.unwrap().is_ok());
}

#[tokio::test]
async fn ping_is_answered_with_pong() {
    let (_client, mut daemon) = pair();
    let ping = Header {
        prog: proto::KEEPALIVE_PROGRAM,
        vers: proto::KEEPALIVE_PROTOCOL_VERSION,
        proc: proto::KEEPALIVE_PROC_PING,
        ty: MessageType::Message,
        serial: 0,
        status: Status::Ok,
    };
    daemon.write(&ping, &[]).await;
    let (h, _) = daemon.read_frame().await;
    assert_eq!(h.prog, proto::KEEPALIVE_PROGRAM);
    assert_eq!(h.proc, proto::KEEPALIVE_PROC_PONG);
}

#[tokio::test]
async fn console_stream_delivers_data_then_eof() {
    let (client, mut daemon) = pair();
    let open = tokio::spawn({
        let c = client.clone();
        async move { c.open_console(&domain(), None, 0).await }
    });
    let (h, _) = daemon.read_frame().await;
    assert_eq!(h.proc, procs::DOMAIN_OPEN_CONSOLE);
    daemon.reply(&h, Status::Ok, &[]).await;
    daemon.stream_msg(&h, Status::Continue, b"login: ").await;
    daemon.stream_msg(&h, Status::Ok, &[]).await;
    let (mut reader, writer) = open.await.unwrap().unwrap().into_split();
    assert_eq!(reader.recv().await.unwrap().unwrap(), b"login: ");
    assert!(reader.recv().await.is_none());

    writer.send(b"root\n").unwrap();
    let (sent, body) = daemon.read_frame().await;
    assert_eq!(sent.ty, MessageType::Stream);
    assert_eq!(sent.status, Status::Continue);
    assert_eq!(body, b"root\n");
    drop(writer);
    let (abort, _) = daemon.read_frame().await;
    assert_eq!(abort.status, Status::Error);
}

#[tokio::test]
async fn stream_error_reaches_the_reader() {
    let (client, mut daemon) = pair();
    let open = tokio::spawn({
        let c = client.clone();
        async move { c.open_console(&domain(), Some("serial0"), 0).await }
    });
    let (h, _) = daemon.read_frame().await;
    daemon.reply(&h, Status::Ok, &[]).await;
    daemon
        .stream_msg(&h, Status::Error, &error_body(1, "console gone"))
        .await;
    let (mut reader, _writer) = open.await.unwrap().unwrap().into_split();
    let err = reader.recv().await.unwrap().unwrap_err();
    assert!(err.to_string().contains("console gone"));
}

#[tokio::test]
async fn failed_open_console_forgets_the_stream() {
    let (client, mut daemon) = pair();
    let open = tokio::spawn({
        let c = client.clone();
        async move { c.open_console(&domain(), None, 0).await }
    });
    let (h, _) = daemon.read_frame().await;
    daemon
        .reply(&h, Status::Error, &error_body(55, "not running"))
        .await;
    assert!(open.await.unwrap().is_err());
    assert!(lock(&client.inner.pending).is_empty());
}

#[tokio::test]
async fn closing_the_socket_fails_pending_calls() {
    let (client, mut daemon) = pair();
    let call = tokio::spawn({
        let c = client.clone();
        async move { c.call(1, Vec::new()).await }
    });
    daemon.read_frame().await;
    drop(daemon);
    assert!(matches!(call.await.unwrap(), Err(Error::Closed)));
    client.closed().await;
    assert!(client.is_closed());
    assert!(matches!(
        client.call(1, Vec::new()).await,
        Err(Error::Closed)
    ));
    assert!(lock(&client.inner.pending).is_empty());
}

#[tokio::test]
async fn call_times_out_and_forgets_the_serial() {
    let (client, mut daemon) = pair();
    client.set_call_timeout(Duration::from_millis(50));
    let err = client.call(1, Vec::new()).await.unwrap_err();
    assert!(matches!(err, Error::Timeout));
    assert!(lock(&client.inner.pending).is_empty());
    // A late reply is ignored and the connection stays usable.
    let (h, _) = daemon.read_frame().await;
    daemon.reply(&h, Status::Ok, &[]).await;
    client.set_call_timeout(Duration::from_secs(5));
    let call = tokio::spawn({
        let c = client.clone();
        async move { c.call(2, Vec::new()).await }
    });
    let (h, _) = daemon.read_frame().await;
    daemon.reply(&h, Status::Ok, &[]).await;
    assert!(call.await.unwrap().is_ok());
}

#[tokio::test(start_paused = true)]
async fn silence_closes_the_connection_with_keepalive_timeout() {
    let (client, _daemon) = pair();
    let call = tokio::spawn({
        let c = client.clone();
        async move { c.call(1, Vec::new()).await }
    });
    tokio::time::sleep(KEEPALIVE_TIMEOUT + Duration::from_secs(1)).await;
    assert!(matches!(call.await.unwrap(), Err(Error::KeepaliveTimeout)));
    assert!(client.is_closed());
}

#[tokio::test]
async fn oversized_arguments_are_rejected_before_sending() {
    let (client, _daemon) = pair();
    let err = client
        .call(1, vec![0; proto::MESSAGE_MAX])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode(_)));
    assert!(lock(&client.inner.pending).is_empty());
}
