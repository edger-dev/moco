use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoDirection {
    Stdin,
    Stdout,
}

#[derive(Debug, Clone)]
pub struct IoChunk {
    pub timestamp: Instant,
    pub direction: IoDirection,
    pub data: Vec<u8>,
}

impl IoChunk {
    pub fn new(direction: IoDirection, data: Vec<u8>) -> Self {
        Self {
            timestamp: Instant::now(),
            direction,
            data,
        }
    }
}

pub type IoCallback = Box<dyn Fn(IoChunk) + Send>;
