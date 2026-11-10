mod support;
use janus_airplay::{Error, testing};
use tokio::{
    io::AsyncReadExt,
    sync::{oneshot, watch},
};

#[tokio::test]
async fn test_stop_acknowledges_teardown_after_completed_feedback() {
    let (client, mut server) = tokio::io::duplex(8192);
    let (stop, _) = watch::channel(false);
    let (stats, mut progress) = watch::channel(janus_airplay::Stats::default());
    let task = tokio::spawn(testing::run_control(client, stop.clone(), stats));
    let (method, path, seq, _) = support::request(&mut server).await.unwrap();
    assert_eq!((method.as_str(), path.as_str()), ("POST", "/feedback"));
    support::respond(&mut server, seq, 200, &[]).await;
    while progress.borrow_and_update().feedback_replies == 0 {
        progress.changed().await.unwrap();
    }
    stop.send_replace(true);
    let (method, _, seq, _) = support::request(&mut server).await.unwrap();
    assert_eq!(method, "TEARDOWN");
    support::respond(&mut server, seq, 200, &[]).await;
    assert_eq!(task.await.unwrap(), (None, true));
}
#[tokio::test]
async fn test_stop_during_partial_control_exchange_closes_without_reuse() {
    let (client, mut server) = tokio::io::duplex(8192);
    let (stop, _) = watch::channel(false);
    let (stats, _) = watch::channel(janus_airplay::Stats::default());
    let task = tokio::spawn(testing::run_control(client, stop.clone(), stats));
    let (method, _, _, _) = support::request(&mut server).await.unwrap();
    assert_eq!(method, "POST");
    stop.send_replace(true);
    assert_eq!(task.await.unwrap(), (None, false));
    assert_eq!(server.read(&mut [0; 8]).await.unwrap(), 0);
}
#[tokio::test]
async fn test_stop_interrupts_blocked_media_write() {
    let (client, mut server) = tokio::io::duplex(32);
    let (stop, rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();
    let task = tokio::spawn(testing::write_packet(client, rx));
    let receiver = tokio::spawn(async move {
        let mut first = [0; 32];
        server.read_exact(&mut first).await.unwrap();
        ready_tx.send(()).unwrap();
        let mut rest = Vec::new();
        server.read_to_end(&mut rest).await.unwrap();
        rest.len()
    });
    ready_rx.await.unwrap();
    stop.send_replace(true);
    assert_eq!(task.await.unwrap(), Err(Error::Stopped));
    assert!(
        receiver.await.unwrap() < 4096 - 32,
        "Stop must interrupt the queued packet"
    );
}
