//! Inspect native Responses failures without changing the supplier's bytes.
//! Lifecycle-only SSE is held until output or a non-failure terminal event;
//! after that point failures are reported, never retried across suppliers.
use serde_json::Value;
use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const MAX_PREFIX: usize = 64 * 1024 * 1024;

pub(super) fn response_failure(value: &Value) -> bool {
    value.get("error").is_some_and(|error| !error.is_null())
        || matches!(
            value.get("status").and_then(Value::as_str),
            Some("failed" | "cancelled")
        )
        || matches!(
            value.get("type").and_then(Value::as_str),
            Some("error" | "response.failed")
        )
        || value.get("response").is_some_and(|response| {
            response.get("error").is_some_and(|error| !error.is_null())
                || matches!(
                    response.get("status").and_then(Value::as_str),
                    Some("failed" | "cancelled")
                )
        })
}

pub(super) struct NativeResponseStream<R: Read> {
    upstream: R,
    pending: Vec<u8>,
    offset: usize,
    frame: Vec<u8>,
    started: bool,
    completed: bool,
    recognized: bool,
    json: Option<bool>,
    eof: bool,
    failed: Arc<AtomicBool>,
}

impl<R: Read> NativeResponseStream<R> {
    pub(super) fn new(upstream: R) -> Self {
        Self {
            upstream,
            pending: Vec::new(),
            offset: 0,
            frame: Vec::new(),
            started: false,
            completed: false,
            recognized: false,
            json: None,
            eof: false,
            failed: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(super) fn failure_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.failed)
    }
    pub(super) fn upstream_mut(&mut self) -> &mut R {
        &mut self.upstream
    }
    fn inspect_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.completed {
            return Ok(());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "Responses SSE 不是有效的 UTF-8")
        })?;
        let mut event = None;
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(value) = line.strip_prefix("event:") {
                event = Some(value.trim());
            }
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        if data.is_empty() {
            return Ok(());
        }
        let data = data.join("\n");
        let value = serde_json::from_str::<Value>(&data).ok();
        let kind = event
            .filter(|event| !event.is_empty())
            .or_else(|| {
                value
                    .as_ref()
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        self.recognized |= kind.starts_with("response.");
        if matches!(kind, "error" | "response.failed")
            || value.as_ref().is_some_and(response_failure)
        {
            self.failed.store(true, Ordering::Release);
            self.completed = true;
            if !self.started {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Responses 上游在输出前返回失败事件",
                ));
            }
        } else if !matches!(
            kind,
            "response.created" | "response.in_progress" | "response.queued"
        ) {
            // Preserve native/legacy gateway output we do not otherwise parse.
            self.started = true;
            if matches!(kind, "response.completed" | "response.incomplete") {
                self.completed = true;
            }
        }
        Ok(())
    }
    fn inspect(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.frame.extend_from_slice(bytes);
        if self.frame.len() > MAX_PREFIX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Responses SSE 事件超过读取上限",
            ));
        }
        while let Some((end, skip)) = frame_end(&self.frame) {
            let block = self.frame[..end].to_vec();
            self.frame.drain(..end + skip);
            self.inspect_frame(&block)?;
        }
        // Some legacy streams expose plain text data without full SSE framing.
        // They still commit output immediately; only recognized lifecycle JSON
        // receives the semantic preflight above.
        if !self.started {
            if let Ok(text) = std::str::from_utf8(&self.frame) {
                let text = text.trim_start();
                if let Some(data) = text.strip_prefix("data:") {
                    let first = data.trim_start().as_bytes().first().copied();
                    if first.is_some_and(|byte| !matches!(byte, b'{' | b'[')) {
                        self.started = true;
                    }
                } else if !text.is_empty()
                    && !text.starts_with(':')
                    && !["event:", "data:", "id:", "retry:"]
                        .iter()
                        .any(|prefix| text.starts_with(prefix) || prefix.starts_with(text))
                {
                    self.started = true;
                }
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for NativeResponseStream<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.started && self.offset < self.pending.len() {
                let count = output.len().min(self.pending.len() - self.offset);
                output[..count].copy_from_slice(&self.pending[self.offset..self.offset + count]);
                self.offset += count;
                if self.offset == self.pending.len() {
                    self.pending.clear();
                    self.offset = 0;
                }
                return Ok(count);
            }
            if self.eof || self.completed {
                return Ok(0);
            }
            let mut bytes = [0; 16 * 1024];
            let count = self.upstream.read(&mut bytes)?;
            if count == 0 {
                self.eof = true;
                if self.json == Some(true) {
                    if serde_json::from_slice::<Value>(&self.pending)
                        .ok()
                        .as_ref()
                        .is_some_and(response_failure)
                    {
                        self.failed.store(true, Ordering::Release);
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Responses 上游返回了 HTTP 200 失败响应",
                        ));
                    }
                    self.started = !self.pending.is_empty();
                } else if !self.frame.is_empty() {
                    let tail = std::mem::take(&mut self.frame);
                    self.inspect_frame(&tail)?;
                }
                if !self.started || (self.recognized && !self.completed) {
                    self.failed.store(true, Ordering::Release);
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Responses 流在输出或完成之前结束",
                    ));
                }
                continue;
            }
            if self.json.is_none() {
                if let Some(first) = bytes[..count]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace())
                {
                    self.json = Some(matches!(*first, b'{' | b'['));
                }
            }
            self.pending.extend_from_slice(&bytes[..count]);
            if !self.started && self.pending.len() > MAX_PREFIX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Responses 首次输出前的数据超过读取上限",
                ));
            }
            if self.json != Some(true) {
                self.inspect(&bytes[..count])?;
            }
        }
    }
}

fn frame_end(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in 0..bytes.len() {
        if bytes.get(index..index + 2) == Some(b"\n\n") {
            return Some((index, 2));
        }
        if bytes.get(index..index + 4) == Some(b"\r\n\r\n") {
            return Some((index, 4));
        }
    }
    None
}
