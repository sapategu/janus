//! Qualification evidence for one exact dependency revision, not a production
//! media adapter or an independent-peer interoperability claim.
mod support;
use anyhow::{Context, Result, ensure};
use std::{sync::Arc, time::Duration};

fn origin() -> moq_net::origin::Producer {
    moq_net::origin::Info::new(moq_net::Origin::random())
        .with_pool(moq_net::cache::Pool::new(4 * 1024 * 1024))
        .with_cache_duration(Duration::from_millis(500))
        .produce()
}

#[tokio::test]
async fn ietf_draft21_object_over_real_quic_and_pre_accept_path() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let version: moq_net::Version = "moq-transport-21".parse().map_err(anyhow::Error::msg)?;
        let network = Arc::new(support::memory_network::Network::default());
        let mut limits = quinn::TransportConfig::default();
        limits.max_concurrent_bidi_streams(16_u8.into());
        limits.max_concurrent_uni_streams(16_u8.into());
        limits.stream_receive_window(262_144_u32.into());
        limits.receive_window(1_048_576_u32.into());
        limits.send_window(1_048_576);
        let limits = Arc::new(limits);
        let mut config = janus_session::tls::server(version.alpn().as_bytes())?;
        config.transport_config(limits.clone());
        let server_endpoint = network.endpoint(45901, Some(config))?;
        let mut endpoint = network.endpoint(45902, None)?;
        let mut config = janus_session::tls::client(version.alpn().as_bytes())?;
        config.transport_config(limits);
        endpoint.set_default_client_config(config);
        let connecting = endpoint.connect(server_endpoint.local_addr()?, "janus.local")?;
        let server_connecting = async {
            server_endpoint
                .accept()
                .await
                .context("no incoming")?
                .await
                .context("server TLS")
        };
        let (client_connection, server_connection) = tokio::try_join!(
            async { connecting.await.map_err(anyhow::Error::from) },
            server_connecting
        )?;
        ensure!(
            client_connection.peer_identity().is_some()
                && server_connection.peer_identity().is_some()
        );
        let publishing = origin();
        let receiving = origin();
        let mut broadcast =
            publishing.create_broadcast("janus/check/1", moq_net::broadcast::Route::announced())?;
        let mut track = broadcast.create_track(
            "video",
            moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO),
        )?;
        let mut group = track.append_group()?;
        group.write_frame(
            moq_net::Timestamp::new(1_000_000, moq_net::Timescale::MICRO)?,
            b"janus-object-check".as_slice(),
        )?;
        group.finish()?;

        let client_builder = moq_net::Client::new()
            .with_versions(version.into())
            .with_publisher(&publishing)
            .with_path("/janus/check/1");
        let server_builder = moq_net::Server::new()
            .with_versions(version.into())
            .with_subscriber(receiving.clone());
        let (client, server) = tokio::try_join!(
            async {
                let (session, driver) = client_builder
                    .connect(web_transport_quinn::Session::raw(client_connection))
                    .await?;
                Ok::<_, anyhow::Error>((session, tokio::spawn(driver)))
            },
            async {
                let request = server_builder
                    .accept_request(web_transport_quinn::Session::raw(server_connection))
                    .await?;
                // Demonstrate that authorization can inspect the path before ok().
                ensure!(
                    request.path() == "/janus/check/1",
                    "request path not exposed before accept"
                );
                let (session, driver) = request.ok().await?;
                Ok::<_, anyhow::Error>((session, tokio::spawn(driver)))
            }
        )?;
        ensure!(client.0.version() == version && server.0.version() == version);
        let client_driver = client.1;
        let server_driver = server.1;
        let consumed = receiving
            .consume()
            .announced_broadcast("janus/check/1")
            .await
            .context("broadcast not announced")?;
        let track = consumed.track("video")?;
        let mut subscriber = track.subscribe(None).await?;
        let mut group = subscriber.next_group().await?.context("group missing")?;
        let frame = group.read_frame().await?.context("object missing")?;
        ensure!(
            frame.payload.as_ref() == b"janus-object-check",
            "object bytes changed"
        );
        client.0.abort(moq_net::Error::Dropped);
        server.0.abort(moq_net::Error::Dropped);
        let _ = client_driver.await?;
        let _ = server_driver.await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
