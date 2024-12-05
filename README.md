# Race DNS Proxy

> ⚠️ **Experimental**: This project is currently in experimental stage and under active development.

A high-performance DNS proxy that races multiple DNS-over-HTTPS (DoH) providers to get the fastest response. Currently supports AliDNS and DNSPod.

## Features

- Concurrent DNS queries to multiple providers
- Automatic failover and retry
- DNS-over-HTTPS (DoH) support
- Smart response selection based on speed and status
- Built with Rust for high performance and reliability

## Usage

Run the proxy server:

```bash
race-dns-proxy [OPTIONS]
```

### Command Line Options

```
Options:
  -p, --port <PORT>    DNS server listening port [default: 5653]
      --log <FILE>     Log filepath
  -h, --help          Print help
  -V, --version       Print version information
```

The server will listen for DNS queries and forward them to configured DoH providers.

## License

MIT License
