fibers_rpc
==========

[![fibers_rpc](http://meritbadge.herokuapp.com/fibers_rpc)](https://crates.io/crates/fibers_rpc)
[![Documentation](https://docs.rs/fibers_rpc/badge.svg)](https://docs.rs/fibers_rpc)
[![Build Status](https://travis-ci.org/sile/fibers_rpc.svg?branch=master)](https://travis-ci.org/sile/fibers_rpc)
[![Code Coverage](https://codecov.io/gh/sile/fibers_rpc/branch/master/graph/badge.svg)](https://codecov.io/gh/sile/fibers_rpc/branch/master)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Rust RPC library built on top of [fibers] crate.

[Documentation](https://docs.rs/fibers_rpc)

[fibers]: https://github.com/dwango/fibers-rs

Features
---------

- Asynchronous RPC server/client using [fibers] crate
- Support two type of RPC:
  - Request/response model
  - Notification model
- Strongly typed RPC using [bytecodec] crate
  - You can treat arbitrarily Rust structures that support [serde] as RPC messages
  - It is possible to handle huge structures as RPC messages without compromising efficiency and real-time property by implementing your own encoder/decoder
- Multiplexing multiple RPC messages in a single TCP stream
- Prioritization between messages
- Expose [Prometheus] metrics

[fibers]: https://github.com/dwango/fibers-rs
[bytecodec]: https://github.com/sile/bytecodec
[serde]: https://crates.io/crates/serde
[Prometheus]: https://prometheus.io/

Technical Details
-----------------

See [doc/].

[doc/]: https://github.com/sile/fibers_rpc/tree/master/doc

Examples
--------

Simple echo RPC server:
```rust
use bytecodec::bytes::{BytesEncoder, RemainingBytesDecoder};
use fibers_rpc::{Call, ProcedureId};
use fibers_rpc::client::ClientServiceBuilder;
use fibers_rpc::server::{HandleCall, Reply, ServerBuilder};
use futures::Future;

// RPC definition
struct EchoRpc;
impl Call for EchoRpc {
    const ID: ProcedureId = ProcedureId(0);
    const NAME: &'static str = "echo";

    type Req = Vec<u8>;
    type ReqEncoder = BytesEncoder<Vec<u8>>;
    type ReqDecoder = RemainingBytesDecoder;

    type Res = Vec<u8>;
    type ResEncoder = BytesEncoder<Vec<u8>>;
    type ResDecoder = RemainingBytesDecoder;
}

// RPC server
struct EchoHandler;
impl HandleCall<EchoRpc> for EchoHandler {
    fn handle_call(&self, request: <EchoRpc as Call>::Req) -> Reply<EchoRpc> {
        Reply::done(request)
    }
}
let server_addr = "127.0.0.1:1919".parse().unwrap();
let mut builder = ServerBuilder::new(server_addr);
builder.add_call_handler(EchoHandler);
let server = builder.finish(fibers_global::handle());
fibers_global::spawn(server.map_err(|e| panic!("{}", e)));

// RPC client
let service = ClientServiceBuilder::new().finish(fibers_global::handle());
let service_handle = service.handle();
fibers_global::spawn(service.map_err(|e| panic!("{}", e)));

let request = Vec::from(&b"hello"[..]);
let response = EchoRpc::client(&service_handle).call(server_addr, request.clone());
let response = fibers_global::execute(response)?;
assert_eq!(response, request);
```

Informal benchmark result (v0.2.1):

```console
$ uname -a
Linux DESKTOP 4.4.0-43-Microsoft #1-Microsoft Wed Dec 31 14:42:53 PST 2014 x86_64 x86_64 x86_64 GNU/Linux

$ lscpu | grep 'Model name:'
Model name:            Intel(R) Core(TM) i7-7660U CPU @ 2.50GHz

// Runs the example echo server in a shell.
$ cargo run --example echo --release -- server

// Executes a benchmark command in another shell.
$ echo "hello" | cargo run --example echo --release -- bench -c 1024 -n 1000000
# ELAPSED: 8.111424
# RPS: 123282.91555218912
```
