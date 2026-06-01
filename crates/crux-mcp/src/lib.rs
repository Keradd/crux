#![deny(unsafe_code)]

pub mod dispatch;
pub mod protocol;
pub mod server;
pub mod shrink;
pub mod tools;

pub use server::{serve_stdio, serve_tcp, serve_tcp_on_listener};
pub use shrink::run_proxy as run_shrink_proxy;
