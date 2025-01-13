# Race DNS Proxy

> ⚠️ **Experimental**: This project is currently in experimental stage and under active development.

A high-performance DNS proxy that races multiple DNS-over-HTTPS (DoH) providers to get the fastest response.

## Features

- Concurrent DNS queries to multiple providers
- Automatic failover and retry
- DNS-over-HTTPS (DoH) support
- Smart response selection based on speed and status
- Built with Rust for high performance and reliability
- Configurable DNS providers via TOML configuration
- Support domain group configuration to specify different DNS servers for different domains

## Dependencies

- [hickory-dns](https://github.com/hickory-dns/hickory-dns): Used for DNS protocol operations and DoH support.
- [tokio](https://github.com/tokio-rs/tokio): Provides the asynchronous runtime for handling concurrent DNS queries.
- [rustls](https://github.com/rustls/rustls): A modern TLS library written in Rust.
- [tracing](https://github.com/tokio-rs/tracing): Application-level tracing framework.
- [webpki-roots](https://github.com/rustls/webpki-roots): Mozilla's CA root certificates for use with webpki.

## Configuration

Create or modify `race-dns-proxy.toml` to configure DNS providers:

```toml
[providers]
[providers.alidns-doh]
addr = "223.5.5.5:443"
hostname = "dns.alidns.com"
domain_groups = ["default"]

[providers.dnspod-doh]
addr = "1.12.12.12:443"
hostname = "doh.pub"
domain_groups = ["default"]

# Domain Groups Configuration
[domain_groups]
default = [] # All domains
```

## Usage

Run the proxy server:

```bash
race-dns-proxy [OPTIONS]
```

### Command Line Options

```
Usage: race-dns-proxy [OPTIONS]

Options:
  -p, --port <PORT>      DNS server listening port [default: 5653]
      --log <LOG>        Log filepath
  -c, --config <CONFIG>  Configuration file path [default: race-dns-proxy.toml]
  -h, --help             Print help
  -V, --version          Print version
```

The server will listen for DNS queries and forward them to configured DoH providers.

## License

This project is Licensed under [MIT License](LICENSE).
