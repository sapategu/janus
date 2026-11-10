use crate::{Coordinator, Grant, Source};
use anyhow::{Context, Result, anyhow, bail, ensure};
use janus_wire::{Code, Empty, Envelope, Hello, Mode, Offer, Payload};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout},
};

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONNECTIONS: usize = 8;

pub struct Server {
    pub address: SocketAddr,
    pub state: Arc<Mutex<Coordinator>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<()>>,
}

impl Server {
    pub async fn start(address: SocketAddr) -> Result<Self> {
        ensure!(
            private_address(address.ip()),
            "bind a concrete private LAN or loopback address"
        );
        let endpoint = quinn::Endpoint::server(crate::tls::server(janus_wire::ALPN)?, address)?;
        Self::from_endpoint(endpoint)
    }

    /// Bind to a caller-supplied endpoint (tests use in-memory packet links).
    #[doc(hidden)]
    pub fn from_endpoint(endpoint: quinn::Endpoint) -> Result<Self> {
        let address = endpoint.local_addr()?;
        let mut coordinator = Coordinator::new(vec![Source::Synthetic]);
        coordinator.set_receiving(true);
        let state = Arc::new(Mutex::new(coordinator));
        let (stop, mut stopped) = watch::channel(false);
        let state_task = state.clone();
        let task = tokio::spawn(async move {
            let started = Instant::now();
            let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            let mut connections = JoinSet::new();
            let mut ticks = tokio::time::interval(Duration::from_millis(20));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    _ = ticks.tick() => { state_task.lock().await.tick(micros(started))?; }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                    incoming = endpoint.accept() => {
                        let Some(incoming) = incoming else { break; };
                        if !private_address(incoming.remote_address().ip()) { incoming.refuse(); continue; }
                        let Ok(permit) = permits.clone().try_acquire_owned() else { incoming.refuse(); continue; };
                        let state = state_task.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            if let Ok(Ok(connection)) = timeout(IO_TIMEOUT, incoming).await {
                                let _ = handle(connection, state, started).await;
                            }
                        });
                    }
                }
            }
            state_task.lock().await.local_stop();
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            endpoint.close(0_u8.into(), b"receiver stopped");
            let _ = timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
            Ok(())
        });
        Ok(Self {
            address,
            state,
            stop,
            task,
        })
    }

    pub async fn shutdown(self) -> Result<()> {
        let _ = self.stop.send(true);
        timeout(Duration::from_secs(2), self.task)
            .await
            .context("receiver shutdown timeout")??
    }
}

pub fn private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}

async fn handle(
    connection: quinn::Connection,
    state: Arc<Mutex<Coordinator>>,
    started: Instant,
) -> Result<()> {
    let mut peer = crate::protocol::ControlPeer::default();
    let result = exchange(&connection, &state, started, &mut peer).await;
    peer.disconnect(&mut *state.lock().await);
    connection.close(0_u8.into(), b"control ended");
    result
}

async fn exchange(
    connection: &quinn::Connection,
    state: &Arc<Mutex<Coordinator>>,
    started: Instant,
    peer: &mut crate::protocol::ControlPeer,
) -> Result<()> {
    let (mut send, mut receive) = timeout(IO_TIMEOUT, connection.accept_bi()).await??;
    loop {
        let request = timeout(IO_TIMEOUT, janus_wire::read(&mut receive)).await??;
        let response = peer.dispatch(&mut *state.lock().await, request, micros(started))?;
        timeout(IO_TIMEOUT, janus_wire::write(&mut send, &response)).await??;
    }
}

pub struct Client {
    _endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    send: quinn::SendStream,
    receive: quinn::RecvStream,
    sequence: u64,
    pub grant: Option<Grant>,
}

impl Client {
    pub async fn connect(address: SocketAddr) -> Result<Self> {
        ensure!(
            private_address(address.ip()),
            "Janus only connects to private LAN/loopback addresses"
        );
        let local = match address {
            SocketAddr::V4(_) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
            SocketAddr::V6(_) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
        };
        let mut endpoint = quinn::Endpoint::client(local)?;
        endpoint.set_default_client_config(crate::tls::client(janus_wire::ALPN)?);
        Self::from_endpoint(endpoint, address).await
    }

    /// Connect over a caller-supplied endpoint (tests use in-memory packet links).
    #[doc(hidden)]
    pub async fn from_endpoint(endpoint: quinn::Endpoint, address: SocketAddr) -> Result<Self> {
        let connection = timeout(IO_TIMEOUT, endpoint.connect(address, "janus.local")?).await??;
        let (send, receive) = timeout(IO_TIMEOUT, connection.open_bi()).await??;
        let mut client = Self {
            _endpoint: endpoint,
            connection,
            send,
            receive,
            sequence: 0,
            grant: None,
        };
        let response = client
            .request(
                Payload::Hello(Hello {
                    major: janus_wire::PROTOCOL_MAJOR,
                }),
                None,
            )
            .await?;
        ensure!(status(&response)? == Code::Ok, "version rejected");
        Ok(client)
    }

    pub async fn request(&mut self, payload: Payload, grant: Option<Grant>) -> Result<Envelope> {
        self.sequence += 1;
        let request = Envelope {
            request_id: self.sequence,
            sequence: self.sequence,
            session_id: grant
                .map(|grant| grant.session.to_vec())
                .unwrap_or_default(),
            generation: grant.map(|grant| grant.generation).unwrap_or(0),
            payload: Some(payload),
            ..Default::default()
        };
        timeout(IO_TIMEOUT, janus_wire::write(&mut self.send, &request)).await??;
        let response = timeout(IO_TIMEOUT, janus_wire::read(&mut self.receive)).await??;
        ensure!(
            response.reply_to == request.request_id && response.sequence == self.sequence,
            "invalid response correlation"
        );
        Ok(response)
    }

    pub async fn offer(&mut self, source: Source) -> Result<Code> {
        self.sequence += 1;
        let mut session = [0; 16];
        self.connection
            .export_keying_material(
                &mut session,
                b"EXPORTER-Janus-Session-v1",
                &self.sequence.to_be_bytes(),
            )
            .map_err(|_| anyhow!("TLS exporter unavailable"))?;
        let mode = match source {
            Source::Synthetic => Mode::Synthetic,
            Source::Video => Mode::Video,
            Source::AudioOnly => Mode::AudioOnly,
        };
        let request = Envelope {
            request_id: self.sequence,
            sequence: self.sequence,
            session_id: session.to_vec(),
            payload: Some(Payload::Offer(Offer { mode: mode as i32 })),
            ..Default::default()
        };
        timeout(IO_TIMEOUT, janus_wire::write(&mut self.send, &request)).await??;
        let response = timeout(IO_TIMEOUT, janus_wire::read(&mut self.receive)).await??;
        ensure!(
            response.reply_to == request.request_id
                && response.sequence == self.sequence
                && response.session_id == session,
            "offer correlation mismatch"
        );
        let code = status(&response)?;
        if code == Code::Ok {
            ensure!(response.generation > 0, "missing generation");
            self.grant = Some(Grant {
                session,
                generation: response.generation,
                source,
            });
        }
        Ok(code)
    }

    pub async fn heartbeat(&mut self) -> Result<Code> {
        let grant = self.grant.context("no session")?;
        let response = self
            .request(Payload::Heartbeat(Empty {}), Some(grant))
            .await?;
        status(&response)
    }

    pub async fn stop(&mut self) -> Result<Code> {
        let grant = self.grant.context("no session")?;
        let response = self.request(Payload::Stop(Empty {}), Some(grant)).await?;
        let code = status(&response)?;
        if code == Code::Ok {
            self.grant = None;
        }
        Ok(code)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.connection.close(0_u8.into(), b"sender ended");
    }
}

pub fn status(response: &Envelope) -> Result<Code> {
    match response.payload.as_ref() {
        Some(Payload::Status(status)) => Ok(Code::try_from(status.code)?),
        _ => bail!("expected status"),
    }
}
