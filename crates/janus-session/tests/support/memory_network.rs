//! Test-only bounded packet links. Quinn still runs its real TLS, congestion,
//! stream and framing implementations; no host sockets or OS network are used.
use quinn::{
    AsyncUdpSocket, UdpPoller,
    udp::{RecvMeta, Transmit},
};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};
use tokio::time::{Duration, Instant, Sleep};

pub const MAX_PACKETS: usize = 256;
const MAX_PACKET_BYTES: usize = 65_535;

#[derive(Debug, Default)]
pub struct Network {
    pub routes: Mutex<HashMap<SocketAddr, Weak<MemorySocket>>>,
    pub impaired: bool,
    pub sent: AtomicUsize,
    pub lost: AtomicUsize,
    pub overflow: AtomicUsize,
    pub queued_bytes: AtomicUsize,
    pub peak_bytes: AtomicUsize,
    pub next_transmit: Mutex<Option<Instant>>,
}

#[derive(Debug, Default)]
pub struct Inbox {
    pub packets: VecDeque<(Instant, SocketAddr, Vec<u8>)>,
    pub wake: Option<Waker>,
    pub timer: Option<Pin<Box<Sleep>>>,
}

#[derive(Debug)]
pub struct MemorySocket {
    pub address: SocketAddr,
    pub network: Arc<Network>,
    pub inbox: Mutex<Inbox>,
}

#[derive(Debug)]
struct Writable;

impl UdpPoller for Writable {
    fn poll_writable(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Network {
    /// Fixed QA profile: shared 8 Mb/s link, 2–5 ms one-way delay/jitter,
    /// every eleventh datagram lost. No host interface or firewall changes.
    pub fn impaired() -> Self {
        Self {
            impaired: true,
            ..Self::default()
        }
    }

    pub fn counters(&self) -> (usize, usize, usize, usize) {
        (
            self.sent.load(Ordering::Relaxed),
            self.lost.load(Ordering::Relaxed),
            self.overflow.load(Ordering::Relaxed),
            self.peak_bytes.load(Ordering::Relaxed),
        )
    }

    pub fn endpoint(
        self: &Arc<Self>,
        port: u16,
        server: Option<quinn::ServerConfig>,
    ) -> io::Result<quinn::Endpoint> {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let socket = Arc::new(MemorySocket {
            address,
            network: self.clone(),
            inbox: Mutex::new(Inbox::default()),
        });
        self.routes
            .lock()
            .unwrap()
            .insert(address, Arc::downgrade(&socket));
        quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            server,
            socket,
            Arc::new(quinn::TokioRuntime),
        )
    }
}

impl AsyncUdpSocket for MemorySocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Writable)
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        let target = self
            .network
            .routes
            .lock()
            .unwrap()
            .get(&transmit.destination)
            .and_then(Weak::upgrade);
        if let Some(target) = target {
            let mut inbox = target.inbox.lock().unwrap();
            let segment = transmit
                .segment_size
                .unwrap_or(transmit.contents.len())
                .max(1);
            for packet in transmit.contents.chunks(segment) {
                let number = self.network.sent.fetch_add(1, Ordering::Relaxed);
                if self.network.impaired && number % 11 == 10 {
                    self.network.lost.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if inbox.packets.len() >= MAX_PACKETS || packet.len() > MAX_PACKET_BYTES {
                    self.network.overflow.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let now = Instant::now();
                let ready = if self.network.impaired {
                    let mut next = self.network.next_transmit.lock().unwrap();
                    let serialized =
                        next.unwrap_or(now).max(now) + Duration::from_micros(packet.len() as u64);
                    *next = Some(serialized);
                    serialized + Duration::from_millis(2 + number as u64 % 4)
                } else {
                    now
                };
                let bytes = self
                    .network
                    .queued_bytes
                    .fetch_add(packet.len(), Ordering::Relaxed)
                    + packet.len();
                self.network.peak_bytes.fetch_max(bytes, Ordering::Relaxed);
                let index = inbox
                    .packets
                    .iter()
                    .position(|(at, _, _)| *at > ready)
                    .unwrap_or(inbox.packets.len());
                inbox
                    .packets
                    .insert(index, (ready, self.address, packet.to_vec()));
                inbox.timer = None; // A newly earlier datagram changes the deadline.
            }
            let wake = inbox.wake.take();
            drop(inbox);
            if let Some(wake) = wake {
                wake.wake();
            }
        }
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buffers: &mut [IoSliceMut<'_>],
        metadata: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut inbox = self.inbox.lock().unwrap();
        let Some((ready, _, _)) = inbox.packets.front() else {
            inbox.wake = Some(cx.waker().clone());
            return Poll::Pending;
        };
        if *ready > Instant::now() {
            let ready = *ready;
            let timer = inbox
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(ready)));
            if timer.as_mut().poll(cx).is_pending() {
                inbox.wake = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        inbox.timer = None;
        let (_, source, packet) = inbox.packets.pop_front().unwrap();
        self.network
            .queued_bytes
            .fetch_sub(packet.len(), Ordering::Relaxed);
        if buffers.is_empty() || metadata.is_empty() || packet.len() > buffers[0].len() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "test packet buffer too small",
            )));
        }
        buffers[0][..packet.len()].copy_from_slice(&packet);
        metadata[0] = RecvMeta {
            addr: source,
            len: packet.len(),
            stride: packet.len(),
            ecn: None,
            dst_ip: Some(self.address.ip()),
        };
        Poll::Ready(Ok(1))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.address)
    }
    fn may_fragment(&self) -> bool {
        false
    }
}

impl Drop for MemorySocket {
    fn drop(&mut self) {
        let bytes = self
            .inbox
            .get_mut()
            .unwrap()
            .packets
            .iter()
            .map(|(_, _, packet)| packet.len())
            .sum();
        self.network
            .queued_bytes
            .fetch_sub(bytes, Ordering::Relaxed);
    }
}
