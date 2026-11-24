//! Live agent connections, request/response correlation and stream fan-out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Notify};
use monitorlab_common::{AgentHello, AgentMsg, HubMsg, MetricsSample, Request, StreamData, StreamRequest};

use crate::util::now_ms;

#[derive(Debug)]
pub enum StreamEvent {
    Data(StreamData),
    End(Option<String>),
}

type Reply = std::result::Result<Value, String>;

pub struct AgentConn {
    pub agent_id: String,
    pub conn_id: u64,
    pub remote: String,
    pub connected_at: i64,
    tx: mpsc::Sender<HubMsg>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    streams: Mutex<HashMap<u64, mpsc::Sender<StreamEvent>>>,
    pub hello: RwLock<Option<AgentHello>>,
    pub latest: RwLock<Option<MetricsSample>>,
    pub last_msg_at: AtomicU64,
    pub shutdown: Notify,
    next_id: AtomicU64,
}

impl AgentConn {
    pub async fn rpc(&self, req: Request, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.tx.send(HubMsg::Request { id, req }).await.is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("agent disconnected");
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!(e)),
            Ok(Err(_)) => Err(anyhow!("agent disconnected")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("agent did not respond within {}s", timeout.as_secs()))
            }
        }
    }

    pub async fn open_stream(self: &Arc<Self>, req: StreamRequest) -> Result<StreamHandle> {
        let sid = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(256);
        self.streams.lock().unwrap().insert(sid, tx);
        if self.tx.send(HubMsg::Open { sid, req }).await.is_err() {
            self.streams.lock().unwrap().remove(&sid);
            bail!("agent disconnected");
        }
        Ok(StreamHandle { sid, conn: self.clone(), rx: Some(rx) })
    }

    pub async fn send(&self, m: HubMsg) -> bool {
        self.tx.send(m).await.is_ok()
    }

    /// Route one message from the agent. Returns the parsed Hello/Metrics for persistence.
    pub async fn dispatch(&self, msg: AgentMsg) -> Option<AgentMsg> {
        self.last_msg_at.store(now_ms() as u64, Ordering::Relaxed);
        match msg {
            AgentMsg::Reply { id, ok, data, error } => {
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(if ok { Ok(data) } else { Err(error.unwrap_or_else(|| "agent error".into())) });
                }
                None
            }
            AgentMsg::Stream { sid, data } => {
                let tx = self.streams.lock().unwrap().get(&sid).cloned();
                match tx {
                    Some(tx) => {
                        // Bounded wait applies backpressure to fast log streams without
                        // letting one stuck browser stall the whole agent connection.
                        let r = tokio::time::timeout(Duration::from_secs(5), tx.send(StreamEvent::Data(data))).await;
                        if !matches!(r, Ok(Ok(()))) {
                            self.streams.lock().unwrap().remove(&sid);
                            let _ = self.tx.send(HubMsg::Close { sid }).await;
                        }
                    }
                    None => {
                        let _ = self.tx.send(HubMsg::Close { sid }).await;
                    }
                }
                None
            }
            AgentMsg::StreamEnd { sid, error } => {
                let tx = self.streams.lock().unwrap().remove(&sid);
                if let Some(tx) = tx {
                    let _ = tx.send(StreamEvent::End(error)).await;
                }
                None
            }
            AgentMsg::Pong => None,
            AgentMsg::Hello(h) => {
                *self.hello.write().unwrap() = Some(h.clone());
                Some(AgentMsg::Hello(h))
            }
            AgentMsg::Metrics(m) => {
                *self.latest.write().unwrap() = Some(m.clone());
                Some(AgentMsg::Metrics(m))
            }
        }
    }

    fn fail_all(&self) {
        self.pending.lock().unwrap().clear();
        let streams: Vec<_> = self.streams.lock().unwrap().drain().map(|(_, tx)| tx).collect();
        for tx in streams {
            let _ = tx.try_send(StreamEvent::End(Some("agent disconnected".into())));
        }
    }
}

/// A live stream; closing (drop) tells the agent to stop.
pub struct StreamHandle {
    pub sid: u64,
    conn: Arc<AgentConn>,
    rx: Option<mpsc::Receiver<StreamEvent>>,
}

impl StreamHandle {
    pub async fn recv(&mut self) -> Option<StreamEvent> {
        self.rx.as_mut()?.recv().await
    }
    pub async fn input(&self, b64: String) -> bool {
        self.conn.send(HubMsg::Input { sid: self.sid, b64 }).await
    }
    pub async fn resize(&self, cols: u16, rows: u16) -> bool {
        self.conn.send(HubMsg::Resize { sid: self.sid, cols, rows }).await
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        let removed = self.conn.streams.lock().unwrap().remove(&self.sid).is_some();
        if removed {
            let _ = self.conn.tx.try_send(HubMsg::Close { sid: self.sid });
        }
    }
}

#[derive(Default)]
pub struct Registry {
    conns: RwLock<HashMap<String, Arc<AgentConn>>>,
    next_conn: AtomicU64,
}

impl Registry {
    /// Register a new connection, replacing (and shutting down) any previous one for the agent.
    pub fn register(&self, agent_id: &str, remote: String, tx: mpsc::Sender<HubMsg>) -> Arc<AgentConn> {
        let conn = Arc::new(AgentConn {
            agent_id: agent_id.to_string(),
            conn_id: self.next_conn.fetch_add(1, Ordering::Relaxed) + 1,
            remote,
            connected_at: now_ms(),
            tx,
            pending: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            hello: RwLock::new(None),
            latest: RwLock::new(None),
            last_msg_at: AtomicU64::new(now_ms() as u64),
            shutdown: Notify::new(),
            next_id: AtomicU64::new(1),
        });
        let old = self.conns.write().unwrap().insert(agent_id.to_string(), conn.clone());
        if let Some(old) = old {
            old.shutdown.notify_one();
            old.fail_all();
        }
        conn
    }

    pub fn unregister(&self, conn: &AgentConn) {
        let mut map = self.conns.write().unwrap();
        if map.get(&conn.agent_id).map(|c| c.conn_id == conn.conn_id).unwrap_or(false) {
            map.remove(&conn.agent_id);
        }
        drop(map);
        conn.fail_all();
    }

    pub fn get(&self, agent_id: &str) -> Option<Arc<AgentConn>> {
        self.conns.read().unwrap().get(agent_id).cloned()
    }

    pub fn disconnect(&self, agent_id: &str) {
        if let Some(c) = self.conns.write().unwrap().remove(agent_id) {
            c.shutdown.notify_one();
            c.fail_all();
        }
    }
}
