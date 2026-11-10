mod support;
use anyhow::{Result, ensure};
use janus_session::Source;
use janus_session::control::*;
use janus_wire::{Code, Empty, Payload};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

#[tokio::test]
async fn impaired_packets_recover_and_stop_without_unbounded_buffers() -> Result<()> {
    timeout(Duration::from_secs(10), async {
        let network = Arc::new(support::memory_network::Network::impaired());
        let endpoint = network.endpoint(45831, Some(janus_session::tls::server(janus_wire::ALPN)?))?;
        let server = Server::from_endpoint(endpoint)?;
        let mut endpoint = network.endpoint(45832, None)?;
        endpoint.set_default_client_config(janus_session::tls::client(janus_wire::ALPN)?);
        let mut client = Client::from_endpoint(endpoint, server.address).await?;
        ensure!(client.offer(Source::Synthetic).await? == Code::Ok);
        for _ in 0..12 {
            ensure!(client.heartbeat().await? == Code::Ok);
        }
        ensure!(client.stop().await? == Code::Ok);
        ensure!(!server.state.lock().await.snapshot().output_permitted);
        let (sent, lost, overflow, peak) = network.counters();
        ensure!(sent > 11 && lost > 0, "impairment never exercised");
        ensure!(overflow == 0 && peak <= 2 * 256 * 65_535, "packet budget exceeded");
        println!("synthetic packet baseline: sent={sent}, lost={lost}, overflow={overflow}, peak_queued_bytes={peak}");
        drop(client);
        server.shutdown().await
    }).await?
}

#[tokio::test]
async fn real_quic_over_memory_packets_enforces_ownership_stop_and_expiry() -> Result<()> {
    timeout(Duration::from_secs(10), async {
        let network = Arc::new(support::memory_network::Network::default());
        let endpoint =
            network.endpoint(45831, Some(janus_session::tls::server(janus_wire::ALPN)?))?;
        let server = Server::from_endpoint(endpoint)?;
        let mut endpoint = network.endpoint(45832, None)?;
        endpoint.set_default_client_config(janus_session::tls::client(janus_wire::ALPN)?);
        let mut first = Client::from_endpoint(endpoint, server.address).await?;
        ensure!(first.offer(Source::Video).await? == Code::Unsupported);
        ensure!(first.offer(Source::Synthetic).await? == Code::Ok);
        let grant = first.grant.unwrap();
        let mut endpoint = network.endpoint(45833, None)?;
        endpoint.set_default_client_config(janus_session::tls::client(janus_wire::ALPN)?);
        let mut second = Client::from_endpoint(endpoint, server.address).await?;
        ensure!(second.offer(Source::Synthetic).await? == Code::Busy);
        let response = second.request(Payload::Stop(Empty {}), Some(grant)).await?;
        ensure!(status(&response)? == Code::Stale);
        ensure!(first.heartbeat().await? == Code::Ok);
        ensure!(first.stop().await? == Code::Ok);
        ensure!(second.offer(Source::Synthetic).await? == Code::Ok);
        tokio::time::sleep(Duration::from_millis(2100)).await;
        ensure!(!server.state.lock().await.snapshot().output_permitted);
        ensure!(second.heartbeat().await? == Code::Expired);
        drop(first);
        drop(second);
        server.shutdown().await
    })
    .await?
}

#[tokio::test]
#[ignore = "requires UDP sockets; run explicitly on an unrestricted native runner"]
async fn real_quic_busy_stop_and_no_native_media_claim() -> Result<()> {
    let server = Server::start("127.0.0.1:0".parse()?).await?;
    let mut first = Client::connect(server.address).await?;
    ensure!(first.offer(Source::Video).await? == Code::Unsupported);
    ensure!(first.offer(Source::Synthetic).await? == Code::Ok);
    let old = first.grant.unwrap();
    let mut second = Client::connect(server.address).await?;
    ensure!(second.offer(Source::Synthetic).await? == Code::Busy);
    ensure!(first.heartbeat().await? == Code::Ok);
    ensure!(first.stop().await? == Code::Ok);
    ensure!(second.offer(Source::Synthetic).await? == Code::Ok);
    let response = first.request(Payload::Stop(Empty {}), Some(old)).await?;
    ensure!(status(&response)? == Code::Ok); // idempotent, cannot stop second
    ensure!(second.heartbeat().await? == Code::Ok);
    ensure!(second.stop().await? == Code::Ok);
    ensure!(!server.state.lock().await.snapshot().output_permitted);
    drop(first);
    drop(second);
    server.shutdown().await
}

#[tokio::test]
#[ignore = "requires UDP sockets; run explicitly on an unrestricted native runner"]
async fn remote_stop_is_bound_to_owning_control_connection() -> Result<()> {
    let server = Server::start("127.0.0.1:0".parse()?).await?;
    let mut owner = Client::connect(server.address).await?;
    ensure!(owner.offer(Source::Synthetic).await? == Code::Ok);
    let grant = owner.grant.unwrap();
    let mut attacker = Client::connect(server.address).await?;
    let response = attacker
        .request(Payload::Stop(Empty {}), Some(grant))
        .await?;
    ensure!(status(&response)? == Code::Stale);
    ensure!(owner.heartbeat().await? == Code::Ok);
    owner.stop().await?;
    drop(owner);
    drop(attacker);
    server.shutdown().await
}

#[tokio::test]
#[ignore = "requires UDP sockets; run explicitly on an unrestricted native runner"]
async fn expired_lease_blanks_without_remote_activity() -> Result<()> {
    let server = Server::start("127.0.0.1:0".parse()?).await?;
    let mut client = Client::connect(server.address).await?;
    client.offer(Source::Synthetic).await?;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    ensure!(!server.state.lock().await.snapshot().output_permitted);
    ensure!(client.heartbeat().await? == Code::Expired);
    drop(client);
    server.shutdown().await
}
